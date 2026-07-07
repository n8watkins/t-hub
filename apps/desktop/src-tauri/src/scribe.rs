// Scribe voice-gate bridge: "is the general dictating right now?"
//
// The general dictates with Scribe (a separate Tauri app). Scribe writes a
// status file at ~/.cache/com.natkins.scribe/status.json with { status,
// listening, since, updatedAt, pid }; `listening` is true while the user is
// actively speaking. T-Hub reads it so voice announcements can HOLD while the
// general talks and DELIVER when they stop (see lib/voiceAnnounce.ts).
//
// FAIL-OPEN doctrine: this returns `listening: false` (which lets T-Hub speak)
// whenever it CANNOT positively confirm an active dictation - a missing file, a
// torn/unparseable file, a stale file (Scribe hasn't refreshed it), a file with
// no pid, or a dead-pid file (the writing Scribe process is gone). Only a file
// that exists, parses, says listening=true, was refreshed recently, AND names a
// live pid holds announcements. The cost of a false "not listening" is talking
// over the general once; the cost of a false "listening" is losing an
// announcement forever - so we err toward speaking.
//
// Path resolution + spawn_blocking mirror voice.rs (HOME/USERPROFILE, with a
// T_HUB_SCRIBE_STATUS_FILE override for tests/E2E).
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Freshness window: a status file whose ON-DISK mtime is older than this is
/// treated as STALE (fail-open). We key freshness off the file mtime rather
/// than the JSON `updatedAt` field so it is agnostic to how Scribe encodes that
/// timestamp (epoch vs ISO). This assumes Scribe REFRESHES the file at least
/// this often while listening (its `updatedAt` is a heartbeat); a generous
/// window so a slightly laggy heartbeat never drops the hold mid-sentence,
/// while still recovering announcements within seconds if Scribe dies on a
/// platform where the pid check is unavailable.
const SCRIBE_STALE_MS: u64 = 15_000;

/// The result T-Hub acts on. `listening` is the COMPUTED effective value (after
/// the fail-open gate); `status` and `since` are passed through from the file
/// (informational, for the MCP tool / logs) when it was readable.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScribeStatus {
    pub listening: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<serde_json::Value>,
}

impl ScribeStatus {
    /// The fail-open result: not listening, nothing to report (missing/torn).
    fn not_listening() -> Self {
        Self::default()
    }
}

/// `~/.cache/com.natkins.scribe/status.json`. `T_HUB_SCRIBE_STATUS_FILE`
/// overrides (mirroring voice.rs's `T_HUB_VOICE_FILE`).
fn scribe_status_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_SCRIBE_STATUS_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("com.natkins.scribe").join("status.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn system_time_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Is a process with this pid alive? On Linux (the primary target + CI + WSL,
/// where the XDG-style `~/.cache` path and the pid share a namespace) this is a
/// cheap `/proc/<pid>` existence check - the same mechanism host.rs/tmux.rs use.
/// On any other platform the pid gate is DISABLED (returns true): a native
/// Windows OpenProcess against a WSL pid would be namespace-meaningless, so
/// staleness (file mtime) is the cross-platform liveness backstop there.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

/// The pure gate: decide `listening` from the parsed status value, the file's
/// mtime, the current time, and a pid-liveness probe. Isolated from IO so the
/// fail-open logic is unit-testable with injected time + pid provider.
fn evaluate(
    v: &serde_json::Value,
    mtime_ms: u64,
    now: u64,
    pid_alive: impl Fn(u32) -> bool,
) -> ScribeStatus {
    let status = v.get("status").and_then(|x| x.as_str()).map(str::to_string);
    let since = v.get("since").cloned();

    let claims_listening = v.get("listening").and_then(|x| x.as_bool()).unwrap_or(false);
    // Fresh = the file was written within the staleness window (mtime, not the
    // JSON timestamp, so encoding never matters).
    let fresh = now.saturating_sub(mtime_ms) <= SCRIBE_STALE_MS;
    // A pid must be present AND alive - a file with no pid cannot be confirmed
    // live, so it fails open.
    let alive = v
        .get("pid")
        .and_then(|x| x.as_u64())
        .map(|p| pid_alive(p as u32))
        .unwrap_or(false);

    ScribeStatus {
        listening: claims_listening && fresh && alive,
        status,
        since,
    }
}

/// Read + evaluate the status file at `path`. Missing, empty, or torn files
/// fail open (not listening). Split from the command wrapper so tests can drive
/// a real temp file without the env override.
fn read_scribe_status_at(path: &Path) -> ScribeStatus {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return ScribeStatus::not_listening(), // missing
    };
    let mtime_ms = meta.modified().map(system_time_ms).unwrap_or(0);
    let raw = match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return ScribeStatus::not_listening(), // empty / unreadable
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return ScribeStatus::not_listening(), // torn
    };
    evaluate(&v, mtime_ms, now_ms(), pid_is_alive)
}

/// Read the current Scribe status from the resolved path. `pub` so the MCP
/// control dispatch (control.rs `scribe_status` arm) can reuse it.
pub fn read_scribe_status() -> ScribeStatus {
    read_scribe_status_at(&scribe_status_path())
}

#[tauri::command]
pub async fn scribe_status() -> Result<ScribeStatus, String> {
    tauri::async_runtime::spawn_blocking(read_scribe_status)
        .await
        .map_err(|e| format!("scribe_status task failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "thub-scribe-test-{}-{}-{tag}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ))
    }

    const ALIVE: fn(u32) -> bool = |_| true;
    const DEAD: fn(u32) -> bool = |_| false;

    #[test]
    fn fresh_alive_listening_is_true() {
        let v = json!({ "listening": true, "status": "dictating", "since": 1, "pid": 42 });
        let s = evaluate(&v, 1_000, 1_000, ALIVE);
        assert!(s.listening);
        assert_eq!(s.status.as_deref(), Some("dictating"));
        assert_eq!(s.since, Some(json!(1)));
    }

    #[test]
    fn stale_file_is_not_listening() {
        let v = json!({ "listening": true, "pid": 42 });
        // mtime is more than the staleness window in the past.
        let s = evaluate(&v, 0, SCRIBE_STALE_MS + 1, ALIVE);
        assert!(!s.listening, "a stale file must fail open");
    }

    #[test]
    fn dead_pid_is_not_listening() {
        let v = json!({ "listening": true, "pid": 42 });
        let s = evaluate(&v, 1_000, 1_000, DEAD);
        assert!(!s.listening, "a dead pid must fail open");
    }

    #[test]
    fn missing_pid_is_not_listening() {
        let v = json!({ "listening": true });
        let s = evaluate(&v, 1_000, 1_000, ALIVE);
        assert!(!s.listening, "no pid cannot be confirmed live");
    }

    #[test]
    fn listening_false_field_is_not_listening() {
        let v = json!({ "listening": false, "pid": 42 });
        let s = evaluate(&v, 1_000, 1_000, ALIVE);
        assert!(!s.listening);
    }

    #[test]
    fn since_and_status_pass_through_even_when_not_listening() {
        // A stale file still surfaces the raw status/since for the MCP tool.
        let v = json!({ "listening": true, "status": "paused", "since": "2026-07-06T00:00:00Z", "pid": 42 });
        let s = evaluate(&v, 0, SCRIBE_STALE_MS + 1, ALIVE);
        assert!(!s.listening);
        assert_eq!(s.status.as_deref(), Some("paused"));
        assert_eq!(s.since, Some(json!("2026-07-06T00:00:00Z")));
    }

    #[test]
    fn missing_file_fails_open() {
        let p = temp_path("missing");
        // Ensure it does not exist.
        let _ = std::fs::remove_file(&p);
        let s = read_scribe_status_at(&p);
        assert!(!s.listening);
        assert!(s.status.is_none());
    }

    #[test]
    fn torn_file_fails_open() {
        let p = temp_path("torn");
        std::fs::write(&p, b"{ not valid json ").unwrap();
        let s = read_scribe_status_at(&p);
        assert!(!s.listening);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn valid_fresh_file_with_own_live_pid_is_listening() {
        // A file we just wrote (fresh mtime) naming THIS process (guaranteed
        // alive) reads as listening end-to-end through the real pid probe.
        let p = temp_path("live");
        let pid = std::process::id();
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "listening": true,
                "status": "dictating",
                "since": 123,
                "updatedAt": 456,
                "pid": pid,
            }))
            .unwrap(),
        )
        .unwrap();
        let s = read_scribe_status_at(&p);
        assert!(s.listening, "fresh file + own live pid should be listening");
        assert_eq!(s.status.as_deref(), Some("dictating"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn path_default_and_env_override() {
        // Default ends with the agreed XDG-style path.
        std::env::remove_var("T_HUB_SCRIBE_STATUS_FILE");
        let def = scribe_status_path();
        assert!(
            def.ends_with("com.natkins.scribe/status.json")
                || def.ends_with("com.natkins.scribe\\status.json"),
            "default path was {def:?}"
        );
    }
}
