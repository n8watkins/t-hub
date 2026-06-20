//! Codex plan usage, read from Codex's session rollout files.
//!
//! Unlike Claude (which has `claude -p /usage`), the Codex CLI exposes no usage
//! command. But it writes `token_count` events into its session JSONL at
//! `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`, and each carries a
//! `rate_limits` block — the SAME shape Claude's statusline reports, just named
//! differently:
//!
//! ```json
//! { "type":"event_msg", "payload": { "type":"token_count",
//!   "info": { "total_token_usage": {...}, "model_context_window": 258400 },
//!   "rate_limits": {
//!     "primary":   { "used_percent": 87, "window_minutes": 300,   "resets_at": 1781157786 },
//!     "secondary": { "used_percent": 14, "window_minutes": 10080, "resets_at": 1781744586 },
//!     "plan_type": "plus" } } }
//! ```
//!
//! Rate limits are ACCOUNT-wide, so the most recent session's latest populated
//! `token_count` reflects current usage — that's what the sidebar shows. We read
//! only the TAIL of the newest session file (recent events live there).
//!
//! Cross-platform like `recent.rs`: unix reads `$HOME/.codex/sessions`; Windows
//! resolves the WSL `$HOME` via `wsl.exe` once and reads over the
//! `\\wsl.localhost\<distro>\...` UNC share.

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
/// true when we got a recognizable `token_count`/`rate_limits` reading at all.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    pub primary: Option<CodexRateWindow>,
    pub secondary: Option<CodexRateWindow>,
    pub plan_type: Option<String>,
    /// Current conversation token count + the model's context window, for a
    /// context-fill hint (last_token_usage.total_tokens / model_context_window).
    pub context_tokens: Option<i64>,
    pub context_window: Option<i64>,
    pub ok: bool,
}

/// Tauri command: read Codex's newest session for the latest usage. Best-effort —
/// returns `CodexUsage { ok: false }` (never errors) so the sidebar degrades.
#[tauri::command]
pub async fn codex_usage() -> Result<CodexUsage, String> {
    Ok(tauri::async_runtime::spawn_blocking(read_codex_usage)
        .await
        .unwrap_or_default())
}

fn read_codex_usage() -> CodexUsage {
    let Some(root) = sessions_root() else {
        return CodexUsage::default();
    };
    let Some(newest) = newest_session(&root) else {
        crate::diag::diag_log(
            "{\"t\":\"codex\",\"m\":\"no session rollout files found\"}".to_string(),
        );
        return CodexUsage::default();
    };
    let tail = read_suffix(&newest, 256 * 1024);
    let usage = parse_latest_usage(&tail);
    crate::diag::diag_log(format!(
        "{{\"t\":\"codex\",\"m\":\"codex_usage ok={} primary={:?} secondary={:?}\"}}",
        usage.ok,
        usage.primary.as_ref().and_then(|w| w.used_percent),
        usage.secondary.as_ref().and_then(|w| w.used_percent),
    ));
    usage
}

/// Scan JSONL tail for the LAST `token_count` event with a populated rate-limit
/// block, falling back to the last one carrying token/context info. A partial
/// first line (we cut mid-file) just fails to parse and is skipped.
fn parse_latest_usage(text: &str) -> CodexUsage {
    let mut best = CodexUsage::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains("token_count") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let payload = v.get("payload");
        if payload.and_then(|p| p.get("type")).and_then(|t| t.as_str()) != Some("token_count") {
            continue;
        }
        let payload = payload.unwrap();
        let mut u = CodexUsage {
            ok: true,
            ..Default::default()
        };
        if let Some(info) = payload.get("info") {
            u.context_window = info
                .get("model_context_window")
                .and_then(serde_json::Value::as_i64);
            u.context_tokens = info
                .get("last_token_usage")
                .and_then(|t| t.get("total_tokens"))
                .and_then(serde_json::Value::as_i64);
        }
        if let Some(rl) = payload.get("rate_limits") {
            u.plan_type = rl
                .get("plan_type")
                .and_then(|p| p.as_str())
                .map(str::to_string);
            u.primary = parse_window(rl.get("primary"));
            u.secondary = parse_window(rl.get("secondary"));
        }
        // Keep the LATEST line; prefer one that actually has a rate-limit window
        // (early-session token_counts report null primary/secondary).
        if u.primary.is_some() || u.secondary.is_some() || best.primary.is_none() {
            best = u;
        }
    }
    best
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
        resets_at: w.get("resets_at").and_then(serde_json::Value::as_i64),
    })
}

/// The most-recently-MODIFIED `rollout-*.jsonl` under `root` (the active session).
/// Walks the year/month/day tree (bounded depth); best-effort.
fn newest_session(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut scanned = 0usize;
    walk(root, 0, &mut scanned, &mut newest);
    newest.map(|(_, p)| p)
}

/// Recursive directory walk capped at depth 4 (sessions are .../yyyy/mm/dd/file)
/// and 5000 entries, tracking the newest `.jsonl` by mtime. Best-effort.
fn walk(
    dir: &std::path::Path,
    depth: usize,
    scanned: &mut usize,
    newest: &mut Option<(std::time::SystemTime, std::path::PathBuf)>,
) {
    if depth > 4 || *scanned > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        *scanned += 1;
        if *scanned > 5000 {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk(&path, depth + 1, scanned, newest);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                *newest = Some((mtime, path));
            }
        }
    }
}

/// Read at most `cap` bytes from the END of a file (lossy UTF-8). Mirrors
/// recent.rs::read_suffix — the latest usage lives at the session's tail.
fn read_suffix(path: &std::path::Path, cap: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(cap as u64);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// --- Cross-platform sessions root (mirrors recent.rs) ----------------------

/// `$HOME/.codex/sessions` on unix; the WSL UNC share on Windows.
#[cfg(not(windows))]
fn sessions_root() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    Some(home.join(".codex").join("sessions"))
}

#[cfg(windows)]
fn sessions_root() -> Option<std::path::PathBuf> {
    let distro = std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    let home = wsl_home(&distro)?;
    let home_rel = home.trim_start_matches('/').replace('/', "\\");
    Some(std::path::PathBuf::from(format!(
        "\\\\wsl.localhost\\{distro}\\{home_rel}\\.codex\\sessions"
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
