//! Codex plan usage, read from Codex's LIVE session rollouts.
//!
//! Unlike Claude (`claude -p /usage`), the Codex CLI exposes no usage command —
//! but the running session writes the account rate-limit block into its per-session
//! rollout (`~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`) on every API response
//! (a `token_count` event). That is the SAME data Codex's `/status` shows, and it
//! is fresh the instant a session makes a call. Each event embeds clean JSON:
//!
//! ```text
//! {"type":"event_msg","payload":{"type":"token_count","info":{…},"rate_limits":{
//!   "primary":  {"used_percent":100,"window_minutes":300,  "resets_at":1782550520},
//!   "secondary":{"used_percent":30, "window_minutes":10080,"resets_at":1782594985},
//!   "credits":{"has_credits":false,…},"plan_type":"plus"}}}
//! ```
//!
//! We read the newest rollout's latest `rate_limits` line. Rate limits are
//! ACCOUNT-wide, so the freshest such event reflects current usage. We FALL BACK
//! to the `~/.codex/logs_*.sqlite` `feedback_log` (the previous source) only when
//! no rollout carries a reading — that DB lags badly (it's only written on certain
//! events), which is why it showed stale numbers when a session was idle/out-of-credits.
//!
//! Cross-platform like `recent.rs`: unix reads `$HOME/.codex`; Windows reads
//! NATIVELY inside WSL (a recursive walk over the `\\wsl.localhost\…` UNC share is
//! slow) and resolves the WSL `$HOME` via `wsl.exe` for the fallback path.

use rusqlite::OpenFlags;
use serde::Serialize;

/// One rate-limit window (`primary` ≈ 5h, `secondary` ≈ weekly). `usedPercent` is
/// 0..=100; the UI shows "left" = 100 - used. `resetsAt` is Unix-epoch seconds.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateWindow {
    pub used_percent: Option<f32>,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
}

/// Parsed Codex usage. Every field optional so a missing block degrades. `ok` is
/// true when we got a recognizable rate-limit reading at all.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    pub primary: Option<CodexRateWindow>,
    pub secondary: Option<CodexRateWindow>,
    pub plan_type: Option<String>,
    pub ok: bool,
}

/// Tauri command: read Codex's newest log DB for the latest usage. Best-effort —
/// returns `CodexUsage { ok: false }` (never errors) so the sidebar degrades.
#[tauri::command]
pub async fn codex_usage() -> Result<CodexUsage, String> {
    Ok(tauri::async_runtime::spawn_blocking(codex_usage_blocking)
        .await
        .unwrap_or_default())
}

/// SYNC Codex usage read — the core of [`codex_usage`] minus the async/`spawn_blocking`
/// wrapper. The control channel calls this (server-split M3) to serve the daemon's
/// Codex plan usage over the socket, so a thin client gets the Codex usage strip
/// remotely. Reads the newest `~/.codex/logs_*.sqlite` rate-limit row on a (blocking)
/// control connection thread — the blocking sqlite/WSL IO is fine there. No cache:
/// it's a single cheap newest-row read.
pub fn codex_usage_blocking() -> CodexUsage {
    read_codex_usage()
}

fn read_codex_usage() -> CodexUsage {
    // Windows: read NATIVELY inside WSL first (a recursive rollout walk + the 100MB+
    // WAL fallback DB over the `\\wsl.localhost\` UNC share stalls and saturates I/O
    // — a freezing contributor). The WSL read is ~0.2s. Only fall through to the UNC
    // path below if WSL/python is unusable.
    #[cfg(windows)]
    if let Some(u) = read_codex_usage_via_wsl() {
        return u;
    }
    let Some(dir) = codex_dir() else {
        return CodexUsage::default();
    };
    // Prefer the freshest LIVE session rollout — the same numbers `/status` shows.
    if let Some(usage) = newest_rollout_usage(&dir) {
        crate::diag::diag_log(format!(
            "{{\"t\":\"codex\",\"m\":\"codex_usage(rollout) ok={} primary={:?} secondary={:?}\"}}",
            usage.ok,
            usage.primary.as_ref().and_then(|w| w.used_percent),
            usage.secondary.as_ref().and_then(|w| w.used_percent),
        ));
        return usage;
    }
    // Fallback: the laggier feedback-log sqlite (only written on certain events).
    let Some(db) = newest_logs_db(&dir) else {
        crate::diag::diag_log(
            "{\"t\":\"codex\",\"m\":\"no rollout rate_limits and no logs_*.sqlite\"}".to_string(),
        );
        return CodexUsage::default();
    };
    let usage = read_db(&db).unwrap_or_default();
    crate::diag::diag_log(format!(
        "{{\"t\":\"codex\",\"m\":\"codex_usage(logdb-fallback) ok={} primary={:?} secondary={:?}\"}}",
        usage.ok,
        usage.primary.as_ref().and_then(|w| w.used_percent),
        usage.secondary.as_ref().and_then(|w| w.used_percent),
    ));
    usage
}

/// Read the freshest session rollout's latest rate-limit snapshot — the SAME
/// account-wide numbers Codex's `/status` shows, written live on every API
/// response (`token_count` events). We narrow to the few most-recently-modified
/// rollouts, then pick the reading with the latest EVENT timestamp — a file's
/// mtime can bump (resuming a session) without a new rate-limits line, so mtime
/// alone could surface a stale reading; the event timestamp can't.
fn newest_rollout_usage(codex_dir: &std::path::Path) -> Option<CodexUsage> {
    let sessions = codex_dir.join("sessions");
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    collect_rollouts(&sessions, &mut files, 0);
    files.sort_by_key(|entry| std::cmp::Reverse(entry.0)); // newest mtime first
    let mut best: Option<(String, CodexUsage)> = None; // (event timestamp, usage)
    for (_, path) in files.iter().take(8) {
        let Some((ts, body)) = last_rate_limits_in(path) else {
            continue;
        };
        let Some(usage) = parse_usage_body(&body) else {
            continue;
        };
        if usage.primary.is_none() && usage.secondary.is_none() {
            continue;
        }
        // ISO-8601 UTC timestamps sort lexicographically == chronologically.
        if best.as_ref().map(|(t, _)| ts >= *t).unwrap_or(true) {
            best = Some((ts, usage));
        }
    }
    best.map(|(_, usage)| usage)
}

/// Recursively collect `rollout-*.jsonl` files (with mtime) under `dir`
/// (`~/.codex/sessions/<yyyy>/<mm>/<dd>/`). Depth-capped — the layout is shallow.
fn collect_rollouts(
    dir: &std::path::Path,
    out: &mut Vec<(std::time::SystemTime, std::path::PathBuf)>,
    depth: u8,
) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ft.is_dir() {
            collect_rollouts(&path, out, depth + 1);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                    out.push((mtime, path));
                }
            }
        }
    }
}

/// The latest `rate_limits` line in a rollout as `(event_timestamp, body)`, where
/// `body` is sliced from `"rate_limits"` onward so [`parse_usage_body`] can extract
/// the object. The rollout is append-only, so the LAST such line is the file's
/// freshest; its `"timestamp"` lets the caller compare across files.
fn last_rate_limits_in(path: &std::path::Path) -> Option<(String, String)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut last: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        if line.contains("\"rate_limits\"") && line.contains("\"used_percent\"") {
            last = Some(line);
        }
    }
    let line = last?;
    let ts = extract_string(&line, "timestamp").unwrap_or_default();
    let idx = line.find("\"rate_limits\"")?;
    Some((ts, line[idx..].to_string()))
}

/// Windows fast path: read the freshest `"rate_limits":{…}` body NATIVELY inside
/// WSL and parse it. Returns `Some(usage)` whenever the WSL read RAN (even if it
/// found no data → `ok=false`), and `None` only when wsl/python is unusable — so
/// the caller falls back to the UNC read just for the truly-unavailable case.
#[cfg(windows)]
fn read_codex_usage_via_wsl() -> Option<CodexUsage> {
    let body = codex_body_via_wsl()?;
    let usage = parse_usage_body(&body).unwrap_or_default();
    crate::diag::diag_log(format!(
        "{{\"t\":\"codex\",\"m\":\"codex_usage(wsl) ok={} bodylen={}\"}}",
        usage.ok,
        body.len()
    ));
    Some(usage)
}

/// Run a tiny reader INSIDE the distro that prints the freshest rate-limits body
/// in ~0.2s. It scans the newest session ROLLOUTS first (the live `/status` data),
/// then falls back to the newest `~/.codex/logs*.sqlite` row (opened immutable — no
/// locks/WAL/shm, so it never touches the live writer). `-e` makes wsl.exe exec
/// bash DIRECTLY; a bare `--` routes through the user's login shell (zsh) instead —
/// see the note on `tmux.rs::pane_info_command`. `None` on any spawn/exit failure.
#[cfg(windows)]
fn codex_body_via_wsl() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let distro = std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    const READER: &str = r#"python3 - <<'PY'
import sqlite3, glob, os, sys, re
home = os.path.expanduser('~')
# 1) Freshest LIVE session rollout rate_limits (what /status shows). Pick the
#    reading with the latest EVENT timestamp across the recent rollouts — a file's
#    mtime can bump without a new rate-limits line, so mtime alone can be stale.
sd = os.path.join(home, '.codex', 'sessions')
files = sorted(glob.glob(os.path.join(sd, '**', 'rollout-*.jsonl'), recursive=True), key=os.path.getmtime)
best_ts, best_body = '', None
for f in reversed(files[-8:]):
    last = None
    try:
        with open(f, errors='replace') as fh:
            for line in fh:
                if '"rate_limits"' in line and '"used_percent"' in line:
                    last = line
    except OSError:
        continue
    if not last:
        continue
    m = re.search(r'"timestamp":"([^"]*)"', last)
    ts = m.group(1) if m else ''
    if ts >= best_ts:
        i = last.find('"rate_limits"'); best_ts, best_body = ts, last[i:i+2000]
if best_body:
    print(best_body); sys.exit(0)
# 2) Fallback: newest feedback-log sqlite row (laggier).
dbs = sorted(glob.glob(os.path.join(home, '.codex', 'logs*.sqlite')), key=os.path.getmtime)
if dbs:
    try:
        c = sqlite3.connect(f'file:{dbs[-1]}?immutable=1', uri=True)
        for (b,) in c.execute("SELECT feedback_log_body FROM logs WHERE feedback_log_body LIKE '%\"rate_limits\":%' ORDER BY rowid DESC LIMIT 1"):
            # Start the slice at the '{' that OPENS the object enclosing rate_limits,
            # so plan_type (a SIBLING that sits BEFORE rate_limits in this DB shape)
            # rides along — matching the rollout path's body shape. Fall back to the
            # rate_limits offset if no enclosing brace is found. End the window at
            # i+2000 (NOT j+2000) so moving the start earlier never shrinks the
            # rate_limits coverage — the object must stay fully inside the slice or
            # extract_object returns None (ok=false).
            i = b.find('"rate_limits":'); j = b.rfind('{', 0, i)
            if j < 0: j = i
            print(b[j:i+2000]); break
    except sqlite3.Error:
        pass
PY"#;
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.arg("-d")
        .arg(&distro)
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(READER)
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                                      // Bounded (LOCAL_IO): a recursive glob over ~/.codex/sessions + an immutable
                                      // SQLite open. Fast normally, but a slow/large tree must not park the
                                      // control-handler thread this runs on (the M3 `codex_usage` read path).
    let out = crate::bounded_exec::output_with_timeout(cmd, crate::bounded_exec::LOCAL_IO_TIMEOUT)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse a codex log `feedback_log_body` into [`CodexUsage`] by extracting its
/// embedded `"rate_limits":{…}` block. `None` when the body has no parseable
/// rate-limits object. Shared by the unix sqlite reader and the Windows WSL reader.
fn parse_usage_body(body: &str) -> Option<CodexUsage> {
    let obj = extract_object(body, "rate_limits")?;
    let rl = serde_json::from_str::<serde_json::Value>(obj).ok()?;
    Some(CodexUsage {
        ok: true,
        plan_type: extract_string(body, "plan_type"),
        primary: parse_window(rl.get("primary")),
        secondary: parse_window(rl.get("secondary")),
    })
}

/// Open the log DB read-only and pull the most recent populated rate-limit block.
fn read_db(db: &std::path::Path) -> Option<CodexUsage> {
    let conn = open_ro(db)?;
    // Newest rows first; scan a small window for the latest with a populated
    // window (early rows can carry null primary/secondary). `rowid` is monotonic
    // insert order, so DESC is "latest first" and stops cheaply at the LIMIT.
    let mut stmt = conn
        .prepare(
            "SELECT feedback_log_body FROM logs \
             WHERE feedback_log_body LIKE '%\"rate_limits\":%' \
             ORDER BY rowid DESC LIMIT 25",
        )
        .ok()?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .ok()?
        .flatten();
    let mut fallback: Option<CodexUsage> = None;
    for body in rows {
        let Some(u) = parse_usage_body(&body) else {
            continue;
        };
        if u.primary.is_some() || u.secondary.is_some() {
            return Some(u); // newest row WITH real window data wins
        }
        if fallback.is_none() {
            fallback = Some(u); // remember the newest parseable (null-window) row
        }
    }
    fallback
}

/// Open SQLite read-only. Tries a normal RO open first (sees the WAL); falls back
/// to `immutable=1` (no locks/shm — robust over the Windows UNC share, at the cost
/// of possibly missing un-checkpointed writes).
fn open_ro(db: &std::path::Path) -> Option<rusqlite::Connection> {
    if let Ok(c) = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        return Some(c);
    }
    let uri = format!("file:{}?immutable=1", db.to_string_lossy());
    rusqlite::Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

fn parse_window(w: Option<&serde_json::Value>) -> Option<CodexRateWindow> {
    let w = w?;
    if w.is_null() {
        return None;
    }
    Some(CodexRateWindow {
        used_percent: w
            .get("used_percent")
            .and_then(serde_json::Value::as_f64)
            .map(|f| f as f32),
        window_minutes: w.get("window_minutes").and_then(serde_json::Value::as_i64),
        // sqlite logs use `reset_at`; the old JSONL used `resets_at` — accept both.
        resets_at: w
            .get("reset_at")
            .or_else(|| w.get("resets_at"))
            .and_then(serde_json::Value::as_i64),
    })
}

/// Extract the balanced JSON object that follows `"<key>":` inside `body` (the log
/// line embeds it in tracing text, so the body isn't valid JSON as a whole).
/// String-aware brace matching so a `{`/`}` inside a string value can't fool it.
fn extract_object<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let after = body.find(&needle)? + needle.len();
    let bytes = body.as_bytes();
    let mut i = after;
    while i < bytes.len() && bytes[i] != b'{' {
        if bytes[i] != b' ' {
            return None; // something other than whitespace before the object
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let start = i;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == b'"' {
                in_str = false;
            }
        } else {
            match ch {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return body.get(start..=i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Extract a simple `"<key>":"value"` string out of the tracing body.
fn extract_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let after = body.find(&needle)? + needle.len();
    let rest = &body[after..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Newest `logs*.sqlite` (by mtime) in the codex dir — the active log DB.
fn newest_logs_db(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("logs") && name.ends_with(".sqlite")) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, entry.path()));
        }
    }
    newest.map(|(_, p)| p)
}

// --- Cross-platform codex dir (mirrors recent.rs) --------------------------

/// `$HOME/.codex` on unix; the WSL UNC share on Windows.
#[cfg(not(windows))]
fn codex_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    Some(home.join(".codex"))
}

#[cfg(windows)]
fn codex_dir() -> Option<std::path::PathBuf> {
    let distro = std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    let home = wsl_home(&distro)?;
    let home_rel = home.trim_start_matches('/').replace('/', "\\");
    Some(std::path::PathBuf::from(format!(
        "\\\\wsl.localhost\\{distro}\\{home_rel}\\.codex"
    )))
}

/// Resolve the WSL `$HOME` by shelling a login bash once (the proven recent.rs /
/// claude::install pattern — a single simple `echo $HOME` arg avoids wsl.exe's
/// multi-arg mangling).
#[cfg(windows)]
fn wsl_home(distro: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg("echo $HOME")
        .creation_flags(0x0800_0000);
    // Bounded (WSL_PROBE): a trivial `echo $HOME` WSL round-trip; sub-second on a
    // healthy host, but a cold/wedged WSL must not park the handler.
    let out = crate::bounded_exec::output_with_timeout(cmd, crate::bounded_exec::WSL_PROBE_TIMEOUT)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if home.is_empty() {
        None
    } else {
        Some(home)
    }
}
