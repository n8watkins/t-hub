// Scribe voice-gate bridge: "is the general dictating right now?"
//
// The general dictates with Scribe (a separate Tauri app). Scribe writes a
// status file - { status, listening, since, updatedAt, pid } - to its Tauri
// app_cache_dir, which is the OS cache dir joined with Scribe's bundle id:
//   Windows: %LOCALAPPDATA%\com.natkins.scribe\status.json
//   Linux:   ~/.cache/com.natkins.scribe/status.json
//   macOS:   ~/Library/Caches/com.natkins.scribe/status.json
// `listening` is true while the user is actively speaking. T-Hub reads it so
// voice announcements HOLD while the general talks and DELIVER when they stop
// (see lib/voiceAnnounce.ts).
//
// We resolve the path via `dirs::cache_dir()` so the reader lands on the real
// file whatever OS T-Hub runs on (it runs as a WINDOWS app, so this must be
// LOCALAPPDATA, not the hardcoded Linux ~/.cache). The general also sometimes
// runs the DEV Scribe build (bundle id com.natkins.scribe.dev); we check both,
// prefer the prod file, and OR their listening states.
//
// LIVENESS over freshness: Scribe writes the file only on state TRANSITIONS,
// not as a heartbeat, so during a long continuous dictation the file keeps
// listening=true with an mtime from when recording started. So we DO NOT gate
// on mtime while we can confirm the writing process is alive: hold while
// listening=true AND its pid is alive (a real OpenProcess check on Windows,
// /proc on Linux). Staleness is only a generous crash backstop (minutes) for
// platforms where the pid cannot be checked.
//
// FAIL-OPEN doctrine: this returns `listening: false` (T-Hub speaks) whenever
// it cannot positively confirm an active dictation - a missing file, a torn
// file, a dead pid, or (on an uncheckable platform) a stale file. The cost of a
// false "not listening" is talking over the general once; the cost of a false
// "listening" is losing an announcement forever - so we err toward speaking.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Scribe's production + dev Tauri bundle ids (its app_cache_dir subfolder).
const SCRIBE_BUNDLE_PROD: &str = "com.natkins.scribe";
const SCRIBE_BUNDLE_DEV: &str = "com.natkins.scribe.dev";

/// Crash backstop for platforms where the pid CANNOT be checked (not Windows or
/// Linux): a status file whose on-disk mtime is older than this is treated as
/// stale (fail-open), so a crashed Scribe can never hold announcements forever.
/// Generous (minutes, not seconds) because Scribe writes on state transitions,
/// not as a heartbeat - a long continuous dictation legitimately keeps an old
/// mtime, and on the real target (Windows) the pid check makes this irrelevant.
const SCRIBE_UNCHECKABLE_STALE_MS: u64 = 5 * 60 * 1000;

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

/// The status file for a given bundle id, under the OS cache dir. `None` when
/// the OS cache dir cannot be resolved (no HOME/LOCALAPPDATA).
fn scribe_status_file_for(bundle: &str) -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join(bundle).join("status.json"))
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

/// Windows pid-liveness via OpenProcess: T-Hub-on-Windows and Scribe-on-Windows
/// share the OS pid namespace, so the pid in the status file is directly
/// checkable. A pid with no process fails to open.
#[cfg(target_os = "windows")]
fn windows_pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // OpenProcess's `binherithandle` is a plain Rust `bool` in windows 0.61.
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

/// Is a process with this pid alive? `Some(true/false)` when the platform can
/// check it (Windows OpenProcess, Linux `/proc/<pid>`); `None` when it cannot,
/// so the caller falls back to the staleness backstop.
fn pid_liveness(pid: u32) -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        Some(windows_pid_alive(pid))
    }
    #[cfg(target_os = "linux")]
    {
        Some(Path::new("/proc").join(pid.to_string()).exists())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// The pure gate: decide `listening` from the parsed status value, the file's
/// mtime, the current time, and a pid-liveness probe (`Some(alive)` when
/// checkable, `None` when not). Isolated from IO so the fail-open + liveness
/// logic is unit-testable with injected time + pid provider.
///
/// listening = the file claims listening AND either
///   - the pid is present and CHECKABLE and alive (mtime ignored - the writer
///     is proven live, so a stale mtime from a long dictation still holds), or
///   - the pid is unavailable/uncheckable AND the file is fresh (the crash
///     backstop for platforms without a pid check).
fn evaluate(
    v: &serde_json::Value,
    mtime_ms: u64,
    now: u64,
    pid_alive: impl Fn(u32) -> Option<bool>,
) -> ScribeStatus {
    let status = v.get("status").and_then(|x| x.as_str()).map(str::to_string);
    let since = v.get("since").cloned();

    let claims_listening = v.get("listening").and_then(|x| x.as_bool()).unwrap_or(false);
    let fresh = now.saturating_sub(mtime_ms) <= SCRIBE_UNCHECKABLE_STALE_MS;
    let live = match v.get("pid").and_then(|x| x.as_u64()) {
        // A checkable pid is authoritative (ignore mtime); a dead one fails open.
        Some(pid) => match pid_alive(pid as u32) {
            Some(alive) => alive,
            // pid present but this platform can't check it: staleness backstop.
            None => fresh,
        },
        // No pid at all: nothing to check, fall back to staleness.
        None => fresh,
    };

    ScribeStatus {
        listening: claims_listening && live,
        status,
        since,
    }
}

/// One candidate file's evaluation plus its `updatedAt` (for the prod-vs-dev
/// freshest tiebreak). `updatedAt` is Scribe's ISO-8601 timestamp string, which
/// sorts lexicographically in chronological order.
struct CandidateEval {
    status: ScribeStatus,
    updated_at: Option<String>,
}

/// Read + evaluate one status file. `None` when missing / empty / torn.
fn eval_candidate(path: &Path) -> Option<CandidateEval> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta.modified().map(system_time_ms).unwrap_or(0);
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let status = evaluate(&v, mtime_ms, now_ms(), pid_liveness);
    let updated_at = v.get("updatedAt").and_then(|x| x.as_str()).map(str::to_string);
    Some(CandidateEval { status, updated_at })
}

/// Combine the prod + dev candidates (passed prod-first): listening is the OR
/// of both (if EITHER is live + listening we hold); the reported status/since
/// come from the freshest candidate by `updatedAt`, preferring prod on a tie or
/// when neither has a comparable timestamp.
fn combine_candidates(cands: &[CandidateEval]) -> ScribeStatus {
    if cands.is_empty() {
        return ScribeStatus::not_listening();
    }
    let listening = cands.iter().any(|c| c.status.listening);
    // cands[0] is prod; only switch to a later candidate with a STRICTLY
    // greater updatedAt, so prod wins ties / missing timestamps.
    let mut chosen = &cands[0];
    for c in &cands[1..] {
        if c.updated_at > chosen.updated_at {
            chosen = c;
        }
    }
    ScribeStatus {
        listening,
        status: chosen.status.status.clone(),
        since: chosen.status.since.clone(),
    }
}

/// Read + evaluate the status file at exactly `path` (no prod/dev search).
/// Missing, empty, or torn files fail open. Used by the `T_HUB_SCRIBE_STATUS_FILE`
/// override + tests.
fn read_scribe_status_at(path: &Path) -> ScribeStatus {
    match eval_candidate(path) {
        Some(c) => c.status,
        None => ScribeStatus::not_listening(),
    }
}

/// Read the current Scribe status. `T_HUB_SCRIBE_STATUS_FILE` overrides to a
/// single exact file (tests / E2E); otherwise the prod + dev cache-dir files
/// are checked. `pub` so the MCP control dispatch (control.rs `scribe_status`
/// arm) can reuse it.
pub fn read_scribe_status() -> ScribeStatus {
    if let Ok(p) = std::env::var("T_HUB_SCRIBE_STATUS_FILE") {
        if !p.trim().is_empty() {
            return read_scribe_status_at(&PathBuf::from(p));
        }
    }
    let mut cands: Vec<CandidateEval> = Vec::new();
    for bundle in [SCRIBE_BUNDLE_PROD, SCRIBE_BUNDLE_DEV] {
        if let Some(path) = scribe_status_file_for(bundle) {
            if let Some(c) = eval_candidate(&path) {
                cands.push(c);
            }
        }
    }
    combine_candidates(&cands)
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

    /// Checkable + alive / dead, and an uncheckable platform (staleness path).
    const ALIVE: fn(u32) -> Option<bool> = |_| Some(true);
    const DEAD: fn(u32) -> Option<bool> = |_| Some(false);
    const UNCHECKABLE: fn(u32) -> Option<bool> = |_| None;

    #[test]
    fn live_pid_holds_regardless_of_mtime() {
        // The long-dictation case: listening=true, pid alive, but the mtime is
        // ancient (Scribe only wrote on the start-of-recording transition).
        let v = json!({ "listening": true, "status": "Recording", "pid": 42 });
        let s = evaluate(&v, 0, 10 * SCRIBE_UNCHECKABLE_STALE_MS, ALIVE);
        assert!(s.listening, "a live pid must hold even with a stale mtime");
    }

    #[test]
    fn dead_pid_is_not_listening_even_when_fresh() {
        let v = json!({ "listening": true, "pid": 42 });
        let s = evaluate(&v, 1_000, 1_000, DEAD);
        assert!(!s.listening, "a dead pid must fail open");
    }

    #[test]
    fn uncheckable_pid_falls_back_to_staleness() {
        let v = json!({ "listening": true, "pid": 42 });
        // Fresh -> holds.
        assert!(evaluate(&v, 1_000, 1_000, UNCHECKABLE).listening);
        // Stale -> fails open.
        assert!(!evaluate(&v, 0, SCRIBE_UNCHECKABLE_STALE_MS + 1, UNCHECKABLE).listening);
    }

    #[test]
    fn missing_pid_falls_back_to_staleness() {
        let v = json!({ "listening": true });
        assert!(evaluate(&v, 1_000, 1_000, ALIVE).listening); // fresh
        assert!(!evaluate(&v, 0, SCRIBE_UNCHECKABLE_STALE_MS + 1, ALIVE).listening); // stale
    }

    #[test]
    fn listening_false_field_is_not_listening() {
        let v = json!({ "listening": false, "pid": 42 });
        assert!(!evaluate(&v, 1_000, 1_000, ALIVE).listening);
    }

    #[test]
    fn since_and_status_pass_through_even_when_not_listening() {
        let v = json!({ "listening": true, "status": "Ready", "since": "2026-07-07T02:05:08Z", "pid": 42 });
        let s = evaluate(&v, 1_000, 1_000, DEAD); // dead -> not listening
        assert!(!s.listening);
        assert_eq!(s.status.as_deref(), Some("Ready"));
        assert_eq!(s.since, Some(json!("2026-07-07T02:05:08Z")));
    }

    #[test]
    fn missing_file_fails_open() {
        let p = temp_path("missing");
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
        // A file naming THIS process (guaranteed alive) reads as listening
        // end-to-end through the real pid probe.
        let p = temp_path("live");
        let pid = std::process::id();
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "listening": true,
                "status": "Recording",
                "since": "2026-07-07T02:00:00Z",
                "updatedAt": "2026-07-07T02:00:00Z",
                "pid": pid,
            }))
            .unwrap(),
        )
        .unwrap();
        let s = read_scribe_status_at(&p);
        assert!(s.listening, "fresh file + own live pid should be listening");
        assert_eq!(s.status.as_deref(), Some("Recording"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn path_resolves_under_the_os_cache_dir() {
        // The tightened assertion (a weak ends_with is what let the LOCALAPPDATA
        // blocker slip through): the resolved path is EXACTLY the OS cache dir
        // joined with Scribe's bundle id + status.json.
        let cache = dirs::cache_dir().expect("OS cache dir");
        assert_eq!(
            scribe_status_file_for(SCRIBE_BUNDLE_PROD),
            Some(cache.join("com.natkins.scribe").join("status.json")),
        );
        assert_eq!(
            scribe_status_file_for(SCRIBE_BUNDLE_DEV),
            Some(cache.join("com.natkins.scribe.dev").join("status.json")),
        );
    }

    #[test]
    fn combine_ors_listening_and_reports_the_freshest() {
        // prod not listening (older), dev listening (newer) -> overall listening,
        // and status/since reported from dev (the freshest updatedAt).
        let prod = CandidateEval {
            status: ScribeStatus { listening: false, status: Some("Ready".into()), since: None },
            updated_at: Some("2026-07-07T01:00:00Z".into()),
        };
        let dev = CandidateEval {
            status: ScribeStatus { listening: true, status: Some("Recording".into()), since: None },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        let combined = combine_candidates(&[prod, dev]);
        assert!(combined.listening, "OR of both -> listening");
        assert_eq!(combined.status.as_deref(), Some("Recording"), "freshest reported");
    }

    #[test]
    fn combine_prefers_prod_on_a_tie() {
        let prod = CandidateEval {
            status: ScribeStatus { listening: false, status: Some("prod".into()), since: None },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        let dev = CandidateEval {
            status: ScribeStatus { listening: false, status: Some("dev".into()), since: None },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        assert_eq!(
            combine_candidates(&[prod, dev]).status.as_deref(),
            Some("prod"),
            "equal updatedAt -> prod wins",
        );
    }

    /// Host verification (the captain's ground truth): when the REAL Scribe
    /// file is present on this machine (via /mnt/c on WSL), the reader FINDS +
    /// parses it and surfaces a status. The general is not dictating right now
    /// so listening must be false. `status` is Scribe's volatile state field
    /// (Ready/Idle/Recording/...), so we assert it is present, not a fixed
    /// value. Skipped on hosts / CI where the file is absent (never flakes).
    #[test]
    fn reads_the_real_scribe_file_on_this_host_when_present() {
        let real = PathBuf::from(
            "/mnt/c/Users/natha/AppData/Local/com.natkins.scribe/status.json",
        );
        if !real.exists() {
            return;
        }
        let s = read_scribe_status_at(&real);
        assert!(!s.listening, "the general is not dictating -> not listening");
        assert!(s.status.is_some(), "the real file's status field was parsed");
    }
}
