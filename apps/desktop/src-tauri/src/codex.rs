//! Codex plan usage, read from Codex's LIVE log database.
//!
//! Unlike Claude (`claude -p /usage`), the Codex CLI exposes no usage command.
//! Older Codex wrote `token_count` events to per-session JSONL rollouts, but that
//! stopped — current Codex logs everything to `~/.codex/logs_*.sqlite`. Each time
//! Codex gets an API response it logs a line whose `feedback_log_body` embeds the
//! account rate-limit block (the body is tracing text, NOT plain JSON, so we
//! extract the `"rate_limits":{…}` object out of it):
//!
//! ```text
//! …codex.op="user_input"…"plan_type":"plus","rate_limits":{
//!   "primary":  {"used_percent":100,"window_minutes":300,  "reset_at":1781157786},
//!   "secondary":{"used_percent":16, "window_minutes":10080,"reset_at":1781744586}}…
//! ```
//!
//! Rate limits are ACCOUNT-wide, so the most recent such row reflects current
//! usage — but only for Codex CLI runs that touch THIS machine's `~/.codex`
//! (a terminal tile in the app). Cloud/web Codex isn't logged here.
//!
//! Cross-platform like `recent.rs`: unix reads `$HOME/.codex`; Windows resolves
//! the WSL `$HOME` via `wsl.exe` and reads over the `\\wsl.localhost\…` UNC share.

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
    Ok(tauri::async_runtime::spawn_blocking(read_codex_usage)
        .await
        .unwrap_or_default())
}

fn read_codex_usage() -> CodexUsage {
    let Some(dir) = codex_dir() else {
        return CodexUsage::default();
    };
    let Some(db) = newest_logs_db(&dir) else {
        crate::diag::diag_log(
            "{\"t\":\"codex\",\"m\":\"no ~/.codex/logs_*.sqlite found\"}".to_string(),
        );
        return CodexUsage::default();
    };
    let usage = read_db(&db).unwrap_or_default();
    crate::diag::diag_log(format!(
        "{{\"t\":\"codex\",\"m\":\"codex_usage ok={} primary={:?} secondary={:?}\"}}",
        usage.ok,
        usage.primary.as_ref().and_then(|w| w.used_percent),
        usage.secondary.as_ref().and_then(|w| w.used_percent),
    ));
    usage
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
        let Some(obj) = extract_object(&body, "rate_limits") else {
            continue;
        };
        let Ok(rl) = serde_json::from_str::<serde_json::Value>(obj) else {
            continue;
        };
        let u = CodexUsage {
            ok: true,
            plan_type: extract_string(&body, "plan_type"),
            primary: parse_window(rl.get("primary")),
            secondary: parse_window(rl.get("secondary")),
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
    let distro = std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
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
    let out = std::process::Command::new("wsl.exe")
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg("echo $HOME")
        .creation_flags(0x0800_0000)
        .output()
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
