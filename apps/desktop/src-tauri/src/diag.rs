//! Runtime diagnostics sink (feat/diag).
//!
//! The app ships as a RELEASE build on Windows with no devtools console the
//! orchestrator (running in WSL) can see. To debug runtime behavior — chiefly
//! the pool's show/park decisions behind the muted/blank-terminal bug — we mirror
//! frontend logs into a FIXED file the orchestrator can `tail` from WSL.
//!
//! Two Tauri commands:
//!   - `diag_log(line)` — append `<ISO-8601 timestamp> <line>\n` to the log.
//!   - `diag_clear()`   — truncate the log so a fresh repro starts clean.
//!
//! The log path is resolved per-user at startup so the WSL-side orchestrator
//! always knows where to read: `$T_HUB_DIAG_FILE` if set (the side-by-side DEV
//! build points this at `~/.t-hub-dev/diag.log`), otherwise `<home>/.t-hub/diag.log`
//! — `%USERPROFILE%` on Windows (readable from WSL under `/mnt/c/...`) and `$HOME`
//! on unix. NOTE: an inherited env var wins, so a prod app launched from a shell
//! that already carries `T_HUB_DIAG_FILE` (e.g. spawned by a dev-isolated app)
//! logs to THAT path — the cause of "prod app writing to the dev diag".
//!
//! Everything here is BEST-EFFORT: we never panic and swallow every IO error, so
//! a missing dir / locked file / full disk can never take down the app or a hot
//! logging path. The `.t-hub` dir is created on demand.
//!
//! Writes are NON-BLOCKING: `diag_log`/`diag_clear` only format the line and
//! hand it to a lazily-spawned background writer thread over an mpsc channel,
//! then return — no file I/O ever runs on the caller's (possibly UI/main)
//! thread. The writer owns ONE open file handle (no reopen-per-line), and caps
//! growth by rotating the file to a single `.1` backup once it exceeds 8 MiB.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{LazyLock, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate the primary log once it grows past this size. A single `.1` backup is
/// kept (overwritten each rotation), so on-disk usage is bounded at ~2x this —
/// the file historically grew unbounded to 100+ MiB under the ~20 always-on
/// hang-detector callers.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// The diagnostics log path, resolved ONCE at startup.
///
/// If `$T_HUB_DIAG_FILE` is set, it is used VERBATIM; otherwise the fixed
/// per-OS default below. The env hook exists so a side-by-side **DEV** instance
/// can write to its OWN diag log (e.g. `T_HUB_DIAG_FILE=.../diag-dev.log`)
/// instead of appending into — and `diag_clear`-truncating — production's log.
/// With NO env var set the path is exactly the previous hard-coded default, so
/// default behavior is byte-for-byte unchanged.
static DIAG_FILE: LazyLock<PathBuf> = LazyLock::new(|| match std::env::var("T_HUB_DIAG_FILE") {
    Ok(p) if !p.is_empty() => PathBuf::from(p),
    _ => default_diag_log_path(),
});

/// The per-user diagnostics log path (the default when `$T_HUB_DIAG_FILE` is
/// unset): `<home>/.t-hub/diag.log`, where `home` is `%USERPROFILE%` on Windows
/// and `$HOME` on unix — resolved at runtime, NOT hardcoded (so it's correct on any
/// machine, not just the dev box). Falls back to the current dir if neither is set.
fn default_diag_log_path() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".t-hub")
        .join("diag.log")
}

/// Emit a one-line startup marker that ALWAYS fires, recording the build version
/// and the RESOLVED diag path. If this line is absent from the log after a launch,
/// diag writes aren't landing (the path is wrong, or the dir isn't writable) —
/// which is exactly the "app runs but diag is stale" symptom to chase.
pub fn log_startup() {
    diag_log(format!(
        "t-hub: started v{} (diag -> {})",
        env!("CARGO_PKG_VERSION"),
        diag_log_path().display()
    ));
}

/// The resolved diagnostics log path (`$T_HUB_DIAG_FILE` or the per-OS
/// default). Read once at startup; cheap to call on the hot logging path.
fn diag_log_path() -> PathBuf {
    DIAG_FILE.clone()
}

/// Best-effort ISO-8601 (UTC) timestamp, e.g. `2026-06-14T17:04:05.123Z`. Pure
/// arithmetic off the Unix epoch so we pull in no chrono/time dependency (keep
/// Cargo.toml untouched beyond the shared `devtools` feature). Falls back to a
/// raw epoch-millis string if the clock is somehow before the epoch.
fn iso8601_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();

    // Days since 1970-01-01 and the time-of-day remainder.
    let days = total_secs / 86_400;
    let tod = total_secs % 86_400;
    let hour = tod / 3600;
    let min = (tod % 3600) / 60;
    let sec = tod % 60;

    // Convert `days` (since 1970-01-01) to a civil Y-M-D using Howard Hinnant's
    // public-domain days_from_civil inverse. Correct across leap years.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, m, d, hour, min, sec, millis
    )
}

/// Control messages to the background writer thread.
enum Msg {
    /// Append this already-formatted line (includes the trailing newline).
    Line(String),
    /// Empty the log so a fresh repro starts clean (serialized with writes).
    Clear,
}

/// The channel into the background writer, created on first use. Holding only
/// the `Sender` keeps the writer alive for the app's lifetime (it's a daemon —
/// never joined). A failed send (writer thread gone) is swallowed at the call
/// site so diagnostics can never panic the app.
static WRITER: OnceLock<Sender<Msg>> = OnceLock::new();

/// Lazily spawn the background writer thread and return its sender. The writer
/// owns ONE open file handle for the app's lifetime and serializes every append
/// and clear, so the hot logging path does no file I/O.
fn writer() -> &'static Sender<Msg> {
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Msg>();
        let path = diag_log_path();
        // Daemon thread: deliberately not joined. It exits when all senders
        // drop, which only happens at process teardown (the static lives for
        // the whole run), so this is an app-lifetime logger.
        std::thread::Builder::new()
            .name("diag-writer".into())
            .spawn(move || writer_loop(path, rx))
            // If the OS refuses the thread, drop the receiver: subsequent sends
            // fail and are swallowed — diagnostics degrade to no-ops, never panic.
            .ok();
        tx
    })
}

/// The background writer loop: owns one open file handle, appends each line, and
/// rotates to a single `.1` backup once the file exceeds [`ROTATE_BYTES`].
fn writer_loop(path: PathBuf, rx: mpsc::Receiver<Msg>) {
    let mut file = open_log(&path);
    // Track the size ourselves so we don't `stat` per line.
    let mut size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());

    for msg in rx {
        match msg {
            Msg::Line(entry) => {
                // Re-open lazily if a previous error left us without a handle.
                if file.is_none() {
                    file = open_log(&path);
                    size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
                }
                if let Some(f) = file.as_mut() {
                    if f.write_all(entry.as_bytes()).is_ok() {
                        size += entry.len() as u64;
                    }
                }
                if size >= ROTATE_BYTES {
                    file = rotate(&path);
                    size = 0;
                }
            }
            Msg::Clear => {
                // Truncate through the same handle so it stays serialized with
                // appends; fall back to reopening if we have no handle. The
                // handle is append-mode, so writes always land at EOF — after
                // `set_len(0)` that's offset 0, no seek needed.
                match file.as_mut() {
                    Some(f) => {
                        let _ = f.set_len(0);
                    }
                    None => file = open_log(&path),
                }
                size = 0;
            }
        }
    }
}

/// Open (creating the `.t-hub` dir + file as needed) the primary log for
/// appending. Returns `None` on any IO error so the caller stays best-effort.
fn open_log(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Rotate the primary log to a single `.1` backup (overwriting any previous
/// backup) and reopen a fresh, empty primary. Best-effort: on any failure we
/// just reopen the primary for append so logging continues.
fn rotate(path: &Path) -> Option<File> {
    // Append ".1" to the FULL filename (e.g. `diag.log` -> `diag.log.1`) rather
    // than replacing the extension, so the primary keeps its `.log` name.
    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    let backup = PathBuf::from(backup);
    // rename() overwrites an existing destination on both unix and Windows
    // (the latter via the std MoveFileEx replace path), so the .1 backup is
    // single and self-overwriting.
    let _ = fs::rename(path, &backup);
    open_log(path)
}

/// Append `<ISO-8601 timestamp> <line>\n` to the diag log. NON-BLOCKING: formats
/// the entry and hands it to the background writer over a channel, then returns
/// — no file I/O on the caller's thread. Best-effort: a failed send (writer
/// gone) is swallowed so a hot logging path can never panic or fail the app.
#[tauri::command]
pub fn diag_log(line: String) {
    let entry = format!("{} {}\n", iso8601_now(), line);
    let _ = writer().send(Msg::Line(entry));
}

/// Truncate the diag log so a fresh repro starts clean. NON-BLOCKING: the clear
/// is serialized with appends on the writer thread. Best-effort: a failed send
/// is swallowed.
#[tauri::command]
pub fn diag_clear() {
    let _ = writer().send(Msg::Clear);
}
