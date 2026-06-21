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
//! The log path is fixed per-OS so the WSL-side orchestrator always knows where
//! to read:
//!   - Windows: `C:\Users\natha\.t-hub\diag.log`
//!     (readable from WSL at `/mnt/c/Users/natha/.t-hub/diag.log`)
//!   - unix:    `/home/natkins/.t-hub/diag.log`
//!
//! Everything here is BEST-EFFORT: we never panic and swallow every IO error, so
//! a missing dir / locked file / full disk can never take down the app or a hot
//! logging path. The `.t-hub` dir is created on demand.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// The fixed per-OS diagnostics log path (the default when `$T_HUB_DIAG_FILE`
/// is unset). Windows points at the user's `C:\Users\natha\.t-hub\diag.log`;
/// unix at the WSL home. The orchestrator reads this same path.
fn default_diag_log_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Users\natha\.t-hub\diag.log")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/home/natkins/.t-hub/diag.log")
    }
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

/// Append `<ISO-8601 timestamp> <line>\n` to the diag log. Best-effort: creates
/// the `.t-hub` dir if missing and swallows every IO error so a hot logging
/// path can never panic or fail the app.
#[tauri::command]
pub fn diag_log(line: String) {
    let path = diag_log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let entry = format!("{} {}\n", iso8601_now(), line);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(entry.as_bytes());
    }
}

/// Truncate the diag log so a fresh repro starts clean. Best-effort: a failure
/// (missing dir, locked file) is swallowed.
#[tauri::command]
pub fn diag_clear() {
    let path = diag_log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Opening with truncate empties the file (or creates an empty one).
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path);
}
