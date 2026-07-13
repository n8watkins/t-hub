// Scribe voice-gate bridge: "is the general dictating right now?"
//
// The general dictates with Scribe (a separate Tauri app). Scribe publishes its
// live dictation state through its v1 dictation-state interface (authoritative
// contract: scribe-app/docs/integrations/dictation-state-contract.md).
//
// PRIMARY (canonical, contract s3/s5): a loopback HTTP endpoint, discovered by
// reading Scribe's discovery file under the user's HOME dir:
//   ~/.scribe/control.json       (prod; %USERPROFILE%\.scribe\ on Windows)
//   ~/.scribe/control.dev.json   (the side-by-side DEV Scribe flavor)
// The file names a baseUrl + readToken; GET <baseUrl>/v1/status returns the
// snapshot { status, dictating, busy, since, updatedAt, pid, ... }. Discovery
// is NEVER cached: every call re-reads control.json and re-connects, so a
// restarted Scribe (new port + token) is picked up immediately and a stale
// cached address can never wedge the gate (the same lesson as t-hub's own
// control.json). scribe_status is a per-request pull polled at ~250ms, so a
// plain GET per call is the right shape; the SSE stream (/v1/events) would
// only add connection state without making the gate faster. The v1 snapshot is
// held to the same 15s `updatedAt` TTL as the fallback (below): a
// wedged-but-serving Scribe frozen on `busy:true` must not hold voice forever
// - stronger than contract s7.1's intrinsic-liveness, per the fail-open
// doctrine.
//
// FALLBACK (contract s6, used ONLY when the endpoint is unavailable): Scribe
// mirrors the same snapshot to a status.json file in its Tauri app_cache_dir:
//   Windows: %LOCALAPPDATA%\com.natkins.scribe[.dev]\status.json
//   Linux:   ~/.cache/com.natkins.scribe[.dev]/status.json
//   macOS:   ~/Library/Caches/com.natkins.scribe[.dev]/status.json
// The file is trusted per contract s7.2: pid alive (authoritative kill-switch)
// AND the snapshot's `updatedAt` FIELD within a 15s TTL (3x Scribe's ~5s
// heartbeat re-write). This replaces the old "a live pid ignores mtime" rule:
// Scribe now heartbeats the file during a long dictation, so live-but-stale
// means a wedged producer, not a long recording.
//
// GATE FIELD: T-Hub holds voice announcements while the general is inside a
// dictation CYCLE, so it gates on the snapshot's level-triggered `busy` flag
// (contract s2 consumer guidance) - the superset of `dictating` that also
// covers Transcribing/Pasting, when speaking would land on top of the
// transcript. The outward field name stays `listening` (t-hub's own stable
// response shape, consumed by voiceAnnounce.ts, announce.sh and the MCP
// scribe_status tool); it is SOURCED from `busy`. The DEPRECATED status.json
// `listening` alias is never read; `dictating` stands in only when a snapshot
// lacks `busy`.
//
// The general sometimes runs the DEV Scribe build alongside prod; we resolve
// both flavors concurrently (each v1-first, then its own file fallback) and OR
// their busy states, reporting status/since from the freshest snapshot.
//
// FAIL-OPEN doctrine (unchanged): this returns `listening: false` (T-Hub
// speaks) whenever it cannot positively confirm an active dictation - endpoint
// refused/closed/unreachable/401, missing or torn control.json or status.json,
// a dead pid, or a stale snapshot. The cost of a false "not listening" is
// talking over the general once; the cost of a false "listening" is losing an
// announcement forever - so we err toward speaking.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Scribe's production + dev Tauri bundle ids: the app_cache_dir subfolder
/// where each flavor's fallback status.json lives.
const SCRIBE_BUNDLE_PROD: &str = "com.natkins.scribe";
const SCRIBE_BUNDLE_DEV: &str = "com.natkins.scribe.dev";

/// Scribe's v1 discovery files under ~/.scribe (contract s5). The DEV flavor
/// writes control.dev.json so the two flavors never clobber each other.
const SCRIBE_CONTROL_PROD: &str = "control.json";
const SCRIBE_CONTROL_DEV: &str = "control.dev.json";

/// Snapshot staleness TTL on the `updatedAt` field (contract s7.2), enforced on
/// BOTH the v1 primary snapshot and the file fallback: Scribe re-writes its
/// state on a ~5s heartbeat while alive, so a snapshot older than 3x that (or
/// one dated in the future) means a wedged/dead producer -> unknown -> fail open.
const SCRIBE_SNAPSHOT_TTL_MS: i64 = 15_000;

/// HTTP budget for the loopback GET /v1/status. Tight on purpose: the endpoint
/// is 127.0.0.1-only and voiceAnnounce polls this whole gate at ~250ms, so a
/// hung server must resolve to fail-open quickly, not park the poll.
const V1_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const V1_OVERALL_TIMEOUT: Duration = Duration::from_millis(750);

/// The result T-Hub acts on. `listening` is the COMPUTED effective gate value,
/// sourced from the snapshot's `busy` flag (after the fail-open rules);
/// `status` and `since` are passed through from the snapshot (informational,
/// for the MCP tool / logs) when one was readable. `source` names the
/// transport that answered ("v1" or "file") - additive + informational only.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScribeStatus {
    pub listening: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

impl ScribeStatus {
    /// The fail-open result: not listening, nothing to report (nothing was
    /// reachable or trustworthy).
    fn not_listening() -> Self {
        Self::default()
    }
}

/// The status file for a given bundle id, under the OS cache dir. `None` when
/// the OS cache dir cannot be resolved (no HOME/LOCALAPPDATA).
fn scribe_status_file_for(bundle: &str) -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join(bundle).join("status.json"))
}

/// A v1 discovery file under ~/.scribe (contract s5). `None` when the home
/// dir cannot be resolved.
fn scribe_control_file_for(name: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".scribe").join(name))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Scribe timestamps are ISO-8601 UTC with millisecond precision and a `Z`
/// suffix, which is valid RFC3339.
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Is a snapshot fresh: is its `updatedAt` present, parseable, not in the
/// future, and no older than the TTL? Shared by the v1 primary and the file
/// fallback (contract s7.2). A FUTURE timestamp is rejected too - a
/// clock-skewed or wedged producer must not defeat the TTL by post-dating its
/// heartbeat (a bare `now - t` via `saturating_sub` reads the future as age 0
/// and would wrongly pass).
fn snapshot_is_fresh(updated_at: Option<&str>, now: i64) -> bool {
    updated_at
        .and_then(parse_rfc3339_ms)
        .is_some_and(|t| t <= now && now - t <= SCRIBE_SNAPSHOT_TTL_MS)
}

/// Windows pid-liveness via OpenProcess: T-Hub-on-Windows and Scribe-on-Windows
/// share the OS pid namespace, so the pid in the snapshot is directly
/// checkable. A pid with no process fails to open.
#[cfg(target_os = "windows")]
fn windows_pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
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
/// so the caller falls back to the TTL guard alone.
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

// ---------------------------------------------------------------------------
// v1 primary: discovery (control.json) + GET /v1/status
// ---------------------------------------------------------------------------

/// What a v1 discovery file resolves to (contract s5).
struct Discovery {
    base_url: String,
    status_path: String,
    read_token: String,
    pid: Option<u64>,
}

/// Parse a discovery payload. `None` when it is unusable: a future major
/// schemaVersion, or a missing/empty baseUrl or readToken (a bad token would
/// only earn a 401 anyway - fail toward the fallback immediately).
fn parse_control(v: &serde_json::Value) -> Option<Discovery> {
    if v.get("schemaVersion")
        .and_then(|x| x.as_u64())
        .is_some_and(|n| n > 1)
    {
        return None;
    }
    let base_url = v
        .get("baseUrl")?
        .as_str()?
        .trim_end_matches('/')
        .to_string();
    let read_token = v.get("readToken")?.as_str()?.to_string();
    if base_url.is_empty() || read_token.is_empty() {
        return None;
    }
    // Contract s5: use the published endpoint path, not a hard-coded one.
    let status_path = v
        .get("endpoints")
        .and_then(|e| e.get("status"))
        .and_then(|x| x.as_str())
        .unwrap_or("/v1/status")
        .to_string();
    Some(Discovery {
        base_url,
        status_path,
        read_token,
        pid: v.get("pid").and_then(|x| x.as_u64()),
    })
}

/// Read + parse one discovery file. Re-read from disk on EVERY call - never
/// cached, so a Scribe restart (new port + token) or failure re-resolves.
fn read_control_at(path: &Path) -> Option<Discovery> {
    let raw = std::fs::read_to_string(path).ok()?;
    let d = parse_control(&serde_json::from_str(&raw).ok()?)?;
    // Contract s5 consumer flow: pid-check discovery itself; a dead producer's
    // leftover file is void (Scribe crashed without cleaning up).
    if let Some(pid) = d.pid {
        if pid_liveness(pid as u32) == Some(false) {
            return None;
        }
    }
    Some(d)
}

/// GET the v1 status snapshot. `None` on ANY failure - refused, closed,
/// timeout, 401/404, or a non-JSON body - which the caller treats as "the v1
/// endpoint is unavailable" (contract s7.1: the belief dies with the socket).
fn fetch_v1_snapshot(d: &Discovery) -> Option<serde_json::Value> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(V1_CONNECT_TIMEOUT)
        .timeout(V1_OVERALL_TIMEOUT)
        .build();
    let url = format!("{}{}", d.base_url, d.status_path);
    let resp = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {}", d.read_token))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    serde_json::from_str(&body).ok()
}

/// Evaluate a v1 snapshot into the gate result. `None` when the payload is not
/// a usable, fresh Scribe v1 snapshot (future schemaVersion, a missing/foreign
/// `app`, a stale/future `updatedAt`, or neither boolean present - e.g. a
/// non-Scribe local server squatting a reused port), so the caller falls
/// through to the file fallback.
fn eval_v1_snapshot(v: &serde_json::Value, now: i64) -> Option<CandidateEval> {
    if v.get("schemaVersion")
        .and_then(|x| x.as_u64())
        .is_some_and(|n| n > 1)
    {
        return None;
    }
    // F2: require `app == "scribe"` present AND equal, not merely "not wrong".
    // Contract 1 makes `app` a constant that is always present, so this rejects
    // zero real snapshots; but it forces a port-squatter that omits `app` (a
    // non-Scribe server answering `{"busy":true}` on a reused ephemeral port,
    // ignoring the bearer token) to fall through to the file fallback rather
    // than force a false `listening:true`.
    if v.get("app").and_then(|x| x.as_str()) != Some("scribe") {
        return None;
    }
    // F1: the same 15s `updatedAt` TTL the file fallback enforces (contract
    // s7.2), applied to the canonical HTTP path too - stronger than contract
    // s7.1's intrinsic-liveness, per t-hub's fail-open doctrine. A
    // wedged-but-serving Scribe (HTTP thread alive, dictation frozen on
    // `busy:true`) would otherwise hold voice indefinitely; a healthy Scribe
    // heartbeats `updatedAt` so this is free. Missing/unparseable/stale/future
    // -> untrusted -> fall through to the fallback (which itself fails open).
    let updated_at = v.get("updatedAt").and_then(|x| x.as_str());
    if !snapshot_is_fresh(updated_at, now) {
        return None;
    }
    // The gate: `busy` (see module doc), with `dictating` standing in only
    // when a snapshot lacks it (`dictating == true` implies `busy == true`).
    let busy = v
        .get("busy")
        .and_then(|x| x.as_bool())
        .or_else(|| v.get("dictating").and_then(|x| x.as_bool()))?;
    Some(CandidateEval {
        status: ScribeStatus {
            listening: busy,
            status: v.get("status").and_then(|x| x.as_str()).map(str::to_string),
            since: v.get("since").cloned(),
            source: Some("v1"),
        },
        updated_at: updated_at.map(str::to_string),
    })
}

// ---------------------------------------------------------------------------
// File fallback: status.json per contract s7.2
// ---------------------------------------------------------------------------

/// The pure s7.2 gate: decide the fallback result from the parsed snapshot,
/// the current time, and a pid-liveness probe (`Some(alive)` when checkable,
/// `None` when not). Isolated from IO so the fail-open + liveness logic is
/// unit-testable with injected time + pid provider.
///
/// The exact s7.2 algorithm:
///   1. (caller) file missing or unparseable -> not dictating
///   2. pid checkable and dead -> not dictating, regardless of contents
///   3. `updatedAt` missing, unparseable, or older than the 15s TTL -> unknown
///      -> not dictating (Scribe heartbeats the file every ~5s while alive)
///   4. otherwise trust the snapshot: gate on `busy` (fallback `dictating`)
fn evaluate_fallback(
    v: &serde_json::Value,
    now: i64,
    pid_alive: impl Fn(u32) -> Option<bool>,
) -> ScribeStatus {
    let status = v.get("status").and_then(|x| x.as_str()).map(str::to_string);
    let since = v.get("since").cloned();
    let fail_open = ScribeStatus {
        listening: false,
        status: status.clone(),
        since: since.clone(),
        source: Some("file"),
    };

    // Step 2: the pid kill-switch. A checkable-and-dead pid overrides
    // everything else in the file. Missing or uncheckable (platforms without a
    // pid probe) leaves the TTL as the only guard.
    if let Some(pid) = v.get("pid").and_then(|x| x.as_u64()) {
        if pid_alive(pid as u32) == Some(false) {
            return fail_open;
        }
    }

    // Step 3: the TTL, on the `updatedAt` FIELD (not the file mtime). Even a
    // live pid with a stale snapshot means a WEDGED producer, not a long
    // dictation - Scribe's heartbeat guarantees freshness while it is healthy.
    // A future `updatedAt` is rejected too (see `snapshot_is_fresh`).
    if !snapshot_is_fresh(v.get("updatedAt").and_then(|x| x.as_str()), now) {
        return fail_open;
    }

    // Step 4: trust the snapshot. Gate on `busy` (see module doc), with
    // `dictating` as the stand-in when absent. The DEPRECATED `listening`
    // alias is deliberately never read.
    let busy = v
        .get("busy")
        .and_then(|x| x.as_bool())
        .or_else(|| v.get("dictating").and_then(|x| x.as_bool()))
        .unwrap_or(false);
    ScribeStatus {
        listening: busy,
        status,
        since,
        source: Some("file"),
    }
}

/// One candidate's evaluation plus its `updatedAt` (for the prod-vs-dev
/// freshest tiebreak). `updatedAt` is Scribe's ISO-8601 timestamp string,
/// which sorts lexicographically in chronological order.
struct CandidateEval {
    status: ScribeStatus,
    updated_at: Option<String>,
}

/// Read + evaluate one fallback status.json. `None` when missing / empty /
/// torn (the caller then reports fail-open not-listening).
fn eval_candidate(path: &Path) -> Option<CandidateEval> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let status = evaluate_fallback(&v, now_ms(), pid_liveness);
    let updated_at = v
        .get("updatedAt")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    Some(CandidateEval { status, updated_at })
}

// ---------------------------------------------------------------------------
// Composition: v1-first per flavor, prod + dev OR'd
// ---------------------------------------------------------------------------

/// Resolve one Scribe flavor (prod or dev), v1-first: the canonical loopback
/// endpoint when its discovery file + GET + payload all check out, else that
/// flavor's status.json fallback, else `None`. Every step re-reads from disk
/// and re-connects - no state is cached across calls or failures.
fn eval_flavor(control: Option<&Path>, status_file: Option<&Path>) -> Option<CandidateEval> {
    if let Some(snap) = control
        .and_then(read_control_at)
        .and_then(|d| fetch_v1_snapshot(&d))
    {
        if let Some(c) = eval_v1_snapshot(&snap, now_ms()) {
            return Some(c);
        }
    }
    status_file.and_then(eval_candidate)
}

/// Combine the prod + dev candidates (passed prod-first): listening is the OR
/// of both (if EITHER flavor is live + busy we hold); the reported
/// status/since/source come from the freshest candidate by `updatedAt`,
/// preferring prod on a tie or when neither has a comparable timestamp.
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
        source: chosen.status.source,
    }
}

/// A non-empty env var as a path (the tests / E2E override seam).
fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var(key) {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => None,
    }
}

/// Read the current Scribe dictation state. Overrides pin the gate to exact
/// files (tests / E2E): `T_HUB_SCRIBE_CONTROL_FILE` a single v1 discovery
/// file, `T_HUB_SCRIBE_STATUS_FILE` a single fallback file; either alone
/// narrows the gate to that source, together they form one complete flavor.
/// Otherwise both Scribe flavors (prod + dev) are resolved v1-first and
/// combined. `pub` so the MCP control dispatch (control.rs `scribe_status`
/// arm) can reuse it.
pub fn read_scribe_status() -> ScribeStatus {
    let control_override = env_path("T_HUB_SCRIBE_CONTROL_FILE");
    let status_override = env_path("T_HUB_SCRIBE_STATUS_FILE");
    if control_override.is_some() || status_override.is_some() {
        return match eval_flavor(control_override.as_deref(), status_override.as_deref()) {
            Some(c) => c.status,
            None => ScribeStatus::not_listening(),
        };
    }

    // Resolve prod + dev CONCURRENTLY. Each flavor's v1 GET can park up to the
    // HTTP timeout, so a sequential resolve would sum them (~1.5s worst case)
    // against the ~250ms poll; two scoped threads bound the wait to a single
    // flavor's timeout. The intermediate collect() starts BOTH threads before
    // either is joined (a lazy map+filter_map would spawn-then-join serially).
    // Order is preserved (prod first) so combine_candidates keeps its
    // prod-wins-a-tie rule; a panicked resolve drops to `None` (fail-open).
    let cands: Vec<CandidateEval> = std::thread::scope(|s| {
        [
            (SCRIBE_CONTROL_PROD, SCRIBE_BUNDLE_PROD),
            (SCRIBE_CONTROL_DEV, SCRIBE_BUNDLE_DEV),
        ]
        .into_iter()
        .map(|(control_name, bundle)| {
            let control = scribe_control_file_for(control_name);
            let status_file = scribe_status_file_for(bundle);
            s.spawn(move || eval_flavor(control.as_deref(), status_file.as_deref()))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect()
    });
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
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "thub-scribe-test-{}-{}-{tag}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ))
    }

    /// Checkable + alive / dead, and an uncheckable platform (TTL-only path).
    const ALIVE: fn(u32) -> Option<bool> = |_| Some(true);
    const DEAD: fn(u32) -> Option<bool> = |_| Some(false);
    const UNCHECKABLE: fn(u32) -> Option<bool> = |_| None;

    /// A fixed snapshot timestamp; tests express "now" as offsets from it.
    const T0: &str = "2026-07-08T12:00:00.000Z";
    fn t0_ms() -> i64 {
        parse_rfc3339_ms(T0).expect("T0 parses")
    }

    /// The current wall clock as a Scribe-style ISO-8601 UTC string, for
    /// fallback files that must read as fresh through the real clock.
    fn iso_now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Fallback-file read as the production path composes it (file only).
    fn read_fallback_at(path: &Path) -> ScribeStatus {
        match eval_candidate(path) {
            Some(c) => c.status,
            None => ScribeStatus::not_listening(),
        }
    }

    // -- v1 snapshot gating --------------------------------------------------

    #[test]
    fn v1_snapshot_gates_on_busy_not_dictating() {
        // Transcribing: mic closed (dictating=false) but the cycle is not done
        // (busy=true) - speaking now would land on top of the transcript.
        let c = eval_v1_snapshot(
            &json!({
                "schemaVersion": 1, "app": "scribe", "status": "Transcribing",
                "dictating": false, "busy": true, "updatedAt": T0,
            }),
            t0_ms() + 1_000,
        )
        .expect("valid snapshot");
        assert!(
            c.status.listening,
            "busy=true must hold even when dictating=false"
        );
        assert_eq!(c.status.source, Some("v1"));
        assert_eq!(c.status.status.as_deref(), Some("Transcribing"));
    }

    #[test]
    fn v1_snapshot_idle_is_not_listening() {
        let c = eval_v1_snapshot(
            &json!({
                "schemaVersion": 1, "app": "scribe", "status": "Idle",
                "dictating": false, "busy": false, "since": T0, "updatedAt": T0,
            }),
            t0_ms(),
        )
        .expect("valid snapshot");
        assert!(!c.status.listening);
        assert_eq!(c.status.status.as_deref(), Some("Idle"));
        assert_eq!(c.status.since, Some(json!(T0)));
    }

    #[test]
    fn v1_snapshot_uses_dictating_when_busy_is_absent() {
        let c = eval_v1_snapshot(
            &json!({ "app": "scribe", "dictating": true, "updatedAt": T0 }),
            t0_ms(),
        )
        .expect("dictating alone is usable");
        assert!(c.status.listening);
    }

    #[test]
    fn v1_snapshot_rejects_unusable_payloads() {
        let now = t0_ms();
        // A future major contract version (fresh, so the TTL is not the reason).
        assert!(eval_v1_snapshot(
            &json!({ "schemaVersion": 2, "app": "scribe", "busy": true, "updatedAt": T0 }),
            now
        )
        .is_none());
        // A non-Scribe local server squatting a reused port.
        assert!(eval_v1_snapshot(
            &json!({ "schemaVersion": 1, "app": "other", "busy": true, "updatedAt": T0 }),
            now
        )
        .is_none());
        // Neither boolean present: not a snapshot at all.
        assert!(eval_v1_snapshot(
            &json!({ "schemaVersion": 1, "app": "scribe", "updatedAt": T0 }),
            now
        )
        .is_none());
    }

    #[test]
    fn v1_snapshot_stale_updated_at_is_rejected() {
        // F1: a wedged-but-serving Scribe frozen on busy:true past the TTL must
        // NOT hold voice - the stale snapshot is untrusted, so the caller falls
        // through to the file fallback (which itself fails open). Contrast the
        // fallback's own stale-guard test below: this proves the canonical HTTP
        // path enforces the same TTL, closing the fail-CLOSED gap the file path
        // never had.
        let v = json!({ "schemaVersion": 1, "app": "scribe", "busy": true, "updatedAt": T0 });
        assert!(
            eval_v1_snapshot(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS + 1).is_none(),
            "stale v1 snapshot -> fall through to the fallback",
        );
        // At exactly the TTL boundary it still holds.
        let c =
            eval_v1_snapshot(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS).expect("fresh at the boundary");
        assert!(c.status.listening);
    }

    #[test]
    fn v1_snapshot_long_dictation_holds_while_heartbeat_is_fresh() {
        // Regression guard for the voice-gate P0 hypothesis (H1): does a LONG
        // continuous dictation defeat the 15s `updatedAt` TTL? It does NOT.
        // Scribe heartbeats `updatedAt` (~5s) independent of state transitions
        // (contract s1/s6, verified live against Scribe 0.7.0), so a recording
        // running for minutes carries a `since` far in the past but an
        // `updatedAt` re-stamped to ~now. The TTL keys off `updatedAt`, not
        // `since`, so the busy snapshot stays trusted and voice stays held.
        let now = t0_ms();
        let v = json!({
            "schemaVersion": 1, "app": "scribe", "status": "Recording",
            "dictating": true, "busy": true,
            // Entered Recording 10 minutes ago (a long dictation)...
            "since": "2026-07-08T11:50:00.000Z",
            // ...but the heartbeat re-stamped updatedAt 2s ago: still fresh.
            "updatedAt": "2026-07-08T11:59:58.000Z",
        });
        let c = eval_v1_snapshot(&v, now).expect("fresh heartbeat within TTL");
        assert!(
            c.status.listening,
            "a long recording holds voice as long as the heartbeat keeps updatedAt fresh",
        );
    }

    #[test]
    fn v1_snapshot_future_updated_at_is_rejected() {
        // F5: a FUTURE updatedAt (a clock-skewed or wedged producer post-dating
        // its heartbeat) must not defeat the TTL - a bare saturating_sub would
        // read it as age 0 (fresh); the `t <= now` guard rejects it.
        let v = json!({ "schemaVersion": 1, "app": "scribe", "busy": true, "updatedAt": T0 });
        assert!(
            eval_v1_snapshot(&v, t0_ms() - 1_000).is_none(),
            "updatedAt in the future -> rejected, never a false busy",
        );
    }

    #[test]
    fn v1_snapshot_missing_app_is_rejected() {
        // F2: a 200 body with NO `app` field is a port-squatter (a non-Scribe
        // server on a reused ephemeral port), not Scribe. Contract 1 always
        // emits `app`, so requiring it present-and-equal rejects zero real
        // snapshots but forces this one to fall through to the file fallback.
        let v = json!({ "schemaVersion": 1, "busy": true, "updatedAt": T0 });
        assert!(
            eval_v1_snapshot(&v, t0_ms()).is_none(),
            "missing app -> not a Scribe snapshot -> fall through",
        );
    }

    // -- v1 discovery (control.json) ------------------------------------------

    #[test]
    fn control_parse_extracts_the_discovery_fields() {
        let d = parse_control(&json!({
            "schemaVersion": 1, "app": "scribe", "pid": 4242,
            "baseUrl": "http://127.0.0.1:52431/",
            "endpoints": { "status": "/v1/status-custom", "events": "/v1/events" },
            "readToken": "tok",
        }))
        .expect("valid discovery");
        assert_eq!(
            d.base_url, "http://127.0.0.1:52431",
            "trailing slash trimmed"
        );
        assert_eq!(
            d.status_path, "/v1/status-custom",
            "published path used, not hard-coded"
        );
        assert_eq!(d.read_token, "tok");
        assert_eq!(d.pid, Some(4242));

        // No endpoints map -> the contract's default path.
        let d = parse_control(&json!({ "baseUrl": "http://127.0.0.1:1", "readToken": "t" }))
            .expect("valid discovery");
        assert_eq!(d.status_path, "/v1/status");
    }

    #[test]
    fn control_parse_rejects_unusable_files() {
        assert!(
            parse_control(&json!({ "readToken": "tok" })).is_none(),
            "no baseUrl"
        );
        assert!(
            parse_control(&json!({ "baseUrl": "http://127.0.0.1:1" })).is_none(),
            "no token"
        );
        assert!(
            parse_control(
                &json!({ "schemaVersion": 2, "baseUrl": "http://127.0.0.1:1", "readToken": "t" })
            )
            .is_none(),
            "future major schemaVersion",
        );
    }

    // -- v1 over a real loopback socket ---------------------------------------

    /// A one-shot loopback HTTP server: accepts a single connection, checks the
    /// bearer token, and answers 200 + the canned body or a 401.
    fn serve_v1_once(body: String, expected_token: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let req = String::from_utf8_lossy(&req).to_string();
                let resp = if req.contains(&format!("Authorization: Bearer {expected_token}")) {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    )
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        port
    }

    fn write_control(port: u16, token: &str, pid: u64) -> PathBuf {
        let p = temp_path("control");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "schemaVersion": 1, "app": "scribe", "pid": pid,
                "transport": "http-sse",
                "baseUrl": format!("http://127.0.0.1:{port}"),
                "endpoints": { "status": "/v1/status", "events": "/v1/events" },
                "readToken": token,
            }))
            .unwrap(),
        )
        .unwrap();
        p
    }

    #[test]
    fn v1_end_to_end_gates_on_busy_over_real_http() {
        // `updatedAt` is a live heartbeat: the v1 path now enforces the 15s TTL
        // through the real clock, so a fixed T0 would read as stale and fall
        // through to the (absent) fallback.
        let body = json!({
            "schemaVersion": 1, "app": "scribe", "status": "Transcribing",
            "dictating": false, "busy": true,
            "since": T0, "updatedAt": iso_now(), "pid": std::process::id(),
        })
        .to_string();
        let port = serve_v1_once(body, "sekret");
        let ctl = write_control(port, "sekret", std::process::id() as u64);
        let c = eval_flavor(Some(&ctl), None).expect("v1 answered");
        assert!(
            c.status.listening,
            "busy=true gates end-to-end over the wire"
        );
        assert_eq!(c.status.source, Some("v1"));
        assert_eq!(c.status.status.as_deref(), Some("Transcribing"));
        let _ = std::fs::remove_file(&ctl);
    }

    #[test]
    fn v1_unreachable_falls_back_to_the_file_then_fails_open() {
        // Grab an ephemeral port and immediately release it: connecting to it
        // is refused, the contract s7.1 "not running" signal.
        let dead_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            l.local_addr().expect("local addr").port()
        };
        let ctl = write_control(dead_port, "sekret", std::process::id() as u64);

        // No fallback file either: nothing to trust -> fail open.
        assert!(
            eval_flavor(Some(&ctl), None).is_none(),
            "unreachable + no file -> fail open"
        );

        // A live fresh fallback file: the s7.2 path answers instead.
        let file = temp_path("fallback");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "status": "Recording", "dictating": true, "busy": true,
                "updatedAt": iso_now(), "pid": std::process::id(),
            }))
            .unwrap(),
        )
        .unwrap();
        let c = eval_flavor(Some(&ctl), Some(&file)).expect("fallback answered");
        assert!(c.status.listening);
        assert_eq!(c.status.source, Some("file"));
        let _ = std::fs::remove_file(&ctl);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn v1_wrong_token_fails_open() {
        let body = json!({ "schemaVersion": 1, "app": "scribe", "busy": true }).to_string();
        let port = serve_v1_once(body, "right-token");
        let ctl = write_control(port, "wrong-token", std::process::id() as u64);
        assert!(
            eval_flavor(Some(&ctl), None).is_none(),
            "401 -> endpoint unusable -> fail open, never a false busy",
        );
        let _ = std::fs::remove_file(&ctl);
    }

    /// Contract s5 consumer flow step 2: a discovery file naming a dead pid is
    /// void - the endpoint is never contacted, even if something answers there.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn control_with_a_dead_pid_is_void() {
        let body = json!({ "schemaVersion": 1, "app": "scribe", "busy": true }).to_string();
        let port = serve_v1_once(body, "sekret");
        // A pid this OS will never have handed out and kept alive.
        let ctl = write_control(port, "sekret", 4_294_967_294);
        assert!(eval_flavor(Some(&ctl), None).is_none());
        let _ = std::fs::remove_file(&ctl);
    }

    #[test]
    fn torn_control_file_falls_through() {
        let p = temp_path("torn-control");
        std::fs::write(&p, b"{ not valid json ").unwrap();
        assert!(eval_flavor(Some(&p), None).is_none());
        let _ = std::fs::remove_file(&p);
    }

    // -- file fallback: the s7.2 algorithm ------------------------------------

    #[test]
    fn fallback_gates_on_busy_not_dictating() {
        let v = json!({ "dictating": false, "busy": true, "status": "Pasting", "updatedAt": T0, "pid": 42 });
        let s = evaluate_fallback(&v, t0_ms() + 1_000, ALIVE);
        assert!(s.listening, "busy alone must hold (Transcribing/Pasting)");
        assert_eq!(s.source, Some("file"));
    }

    #[test]
    fn fallback_uses_dictating_when_busy_is_absent() {
        let v = json!({ "dictating": true, "updatedAt": T0, "pid": 42 });
        assert!(evaluate_fallback(&v, t0_ms(), ALIVE).listening);
    }

    #[test]
    fn fallback_ignores_the_deprecated_listening_alias() {
        // A pre-v1 file carries only `listening`. Per the contract new
        // consumers MUST read `dictating` - the alias is about to be removed -
        // so it is never trusted. Fail-open direction: with a very old Scribe
        // t-hub may speak over the general once, never lose a cue.
        let v = json!({ "listening": true, "updatedAt": T0, "pid": 42 });
        assert!(!evaluate_fallback(&v, t0_ms(), ALIVE).listening);
    }

    #[test]
    fn fallback_dead_pid_fails_open_even_when_fresh() {
        let v = json!({ "dictating": true, "busy": true, "updatedAt": T0, "pid": 42 });
        assert!(
            !evaluate_fallback(&v, t0_ms(), DEAD).listening,
            "s7.2 step 2: dead pid overrides everything"
        );
    }

    #[test]
    fn fallback_stale_updated_at_fails_open_even_with_a_live_pid() {
        // The s7.2 flip from the old mtime rule: Scribe heartbeats status.json
        // every ~5s while alive, so live-pid-but-stale-snapshot now means a
        // WEDGED producer, not a long dictation - it must fail open.
        let v = json!({ "dictating": true, "busy": true, "updatedAt": T0, "pid": 42 });
        assert!(!evaluate_fallback(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS + 1, ALIVE).listening);
        // At exactly the TTL the snapshot still holds.
        assert!(evaluate_fallback(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS, ALIVE).listening);
    }

    #[test]
    fn fallback_missing_or_bad_updated_at_fails_open() {
        assert!(
            !evaluate_fallback(&json!({ "dictating": true, "pid": 42 }), t0_ms(), ALIVE).listening
        );
        assert!(
            !evaluate_fallback(
                &json!({ "dictating": true, "updatedAt": "not-a-time", "pid": 42 }),
                t0_ms(),
                ALIVE
            )
            .listening
        );
    }

    #[test]
    fn fallback_uncheckable_pid_relies_on_the_ttl_alone() {
        let v = json!({ "dictating": true, "busy": true, "updatedAt": T0, "pid": 42 });
        assert!(evaluate_fallback(&v, t0_ms() + 1_000, UNCHECKABLE).listening);
        assert!(
            !evaluate_fallback(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS + 1, UNCHECKABLE).listening
        );
    }

    #[test]
    fn fallback_missing_pid_relies_on_the_ttl_alone() {
        let v = json!({ "dictating": true, "busy": true, "updatedAt": T0 });
        assert!(evaluate_fallback(&v, t0_ms() + 1_000, ALIVE).listening);
        assert!(!evaluate_fallback(&v, t0_ms() + SCRIBE_SNAPSHOT_TTL_MS + 1, ALIVE).listening);
    }

    #[test]
    fn status_and_since_pass_through_even_when_failing_open() {
        let v = json!({ "dictating": true, "status": "Recording", "since": T0, "updatedAt": T0, "pid": 42 });
        let s = evaluate_fallback(&v, t0_ms(), DEAD);
        assert!(!s.listening);
        assert_eq!(s.status.as_deref(), Some("Recording"));
        assert_eq!(s.since, Some(json!(T0)));
    }

    // -- files + paths ---------------------------------------------------------

    #[test]
    fn missing_file_fails_open() {
        let p = temp_path("missing");
        let _ = std::fs::remove_file(&p);
        let s = read_fallback_at(&p);
        assert!(!s.listening);
        assert!(s.status.is_none());
    }

    #[test]
    fn torn_file_fails_open() {
        let p = temp_path("torn");
        std::fs::write(&p, b"{ not valid json ").unwrap();
        assert!(!read_fallback_at(&p).listening);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn valid_fresh_file_with_own_live_pid_is_listening() {
        // A file naming THIS process (guaranteed alive) with a just-now
        // heartbeat reads as listening end-to-end through the real pid probe
        // and the real clock.
        let p = temp_path("live");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "status": "Recording", "dictating": true, "busy": true,
                "since": iso_now(), "updatedAt": iso_now(),
                "pid": std::process::id(),
            }))
            .unwrap(),
        )
        .unwrap();
        let s = read_fallback_at(&p);
        assert!(
            s.listening,
            "fresh heartbeat + own live pid should be listening"
        );
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
    fn control_resolves_under_home_dot_scribe() {
        // Contract s5: ~/.scribe/control.json (prod) and control.dev.json
        // (dev), under the HOME dir - %USERPROFILE% on Windows, not the cache
        // dir the fallback file lives in.
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(
            scribe_control_file_for(SCRIBE_CONTROL_PROD),
            Some(home.join(".scribe").join("control.json")),
        );
        assert_eq!(
            scribe_control_file_for(SCRIBE_CONTROL_DEV),
            Some(home.join(".scribe").join("control.dev.json")),
        );
    }

    // -- prod + dev combination -------------------------------------------------

    #[test]
    fn combine_ors_listening_and_reports_the_freshest() {
        // prod not busy (older), dev busy (newer) -> overall listening, and
        // status/since reported from dev (the freshest updatedAt).
        let prod = CandidateEval {
            status: ScribeStatus {
                listening: false,
                status: Some("Ready".into()),
                since: None,
                source: Some("v1"),
            },
            updated_at: Some("2026-07-07T01:00:00Z".into()),
        };
        let dev = CandidateEval {
            status: ScribeStatus {
                listening: true,
                status: Some("Recording".into()),
                since: None,
                source: Some("file"),
            },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        let combined = combine_candidates(&[prod, dev]);
        assert!(combined.listening, "OR of both -> listening");
        assert_eq!(
            combined.status.as_deref(),
            Some("Recording"),
            "freshest reported"
        );
        assert_eq!(
            combined.source,
            Some("file"),
            "source follows the chosen candidate"
        );
    }

    #[test]
    fn combine_prefers_prod_on_a_tie() {
        let prod = CandidateEval {
            status: ScribeStatus {
                listening: false,
                status: Some("prod".into()),
                since: None,
                source: Some("v1"),
            },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        let dev = CandidateEval {
            status: ScribeStatus {
                listening: false,
                status: Some("dev".into()),
                since: None,
                source: Some("v1"),
            },
            updated_at: Some("2026-07-07T02:00:00Z".into()),
        };
        assert_eq!(
            combine_candidates(&[prod, dev]).status.as_deref(),
            Some("prod"),
            "equal updatedAt -> prod wins",
        );
    }

    /// F2 golden cross-check: the SAME decision matrix drives both this Rust
    /// suite and the shell gate conformance test (scripts/announce_gate.test.sh
    /// via `announce.sh --gate`). Feeding one shared fixtures file to BOTH the
    /// v1 evaluator and the file-fallback evaluator here - and to the shell gate
    /// there - means any divergence from the golden `hold` value turns a test
    /// red, so contract drift between the Rust and shell implementations cannot
    /// land silently. Fixtures carry `app=="scribe"` and omit `schemaVersion`,
    /// so the v1 evaluator's extra guards are satisfied and it must agree with
    /// the file evaluator on every row.
    #[test]
    fn gate_matches_golden_fixtures_cross_impl() {
        use chrono::TimeZone;
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/gate-fixtures.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let manifest: serde_json::Value = serde_json::from_str(&raw).expect("fixtures parse");
        let base =
            parse_rfc3339_ms(manifest["base"].as_str().expect("base string")).expect("base parses");
        // The fixtures pin the shared TTL; if scribe.rs's TTL ever changes, the
        // manifest (and the shell) must change with it - assert they still match.
        assert_eq!(
            manifest["ttlMs"].as_i64(),
            Some(SCRIBE_SNAPSHOT_TTL_MS),
            "fixtures ttlMs must track SCRIBE_SNAPSHOT_TTL_MS",
        );
        let cases = manifest["cases"].as_array().expect("cases array");
        assert!(!cases.is_empty(), "fixtures must have cases");
        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            let offset = case["offsetMs"].as_i64().expect("offsetMs");
            let hold = case["hold"].as_bool().expect("hold");
            // updatedAt = now - offset (its age); a negative offset dates it in
            // the future. `now` is the fixed `base`, so freshness is exact here.
            let updated_at = chrono::Utc
                .timestamp_millis_opt(base - offset)
                .single()
                .expect("valid ts")
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let mut snap = case["snapshot"].clone();
            snap["updatedAt"] = serde_json::Value::String(updated_at);

            // File-fallback evaluator: pid ALIVE so only the TTL + booleans decide.
            let file_hold = evaluate_fallback(&snap, base, ALIVE).listening;
            assert_eq!(file_hold, hold, "{name}: evaluate_fallback hold != golden");

            // v1 evaluator: None (fall-through) counts as not-holding.
            let v1_hold = eval_v1_snapshot(&snap, base)
                .map(|c| c.status.listening)
                .unwrap_or(false);
            assert_eq!(v1_hold, hold, "{name}: eval_v1_snapshot hold != golden");
        }
    }

    /// Host verification (the captain's ground truth): when the REAL Scribe
    /// fallback file is present on this machine (via /mnt/c on WSL), the
    /// reader FINDS + parses it and surfaces a status. The general is not
    /// dictating right now so listening must be false (idle snapshot, dead
    /// pid, or a stale heartbeat all resolve that way). `status` is Scribe's
    /// volatile state field, so we assert presence, not a fixed value.
    /// Skipped on hosts / CI where the file is absent (never flakes).
    #[test]
    fn reads_the_real_scribe_file_on_this_host_when_present() {
        let real = PathBuf::from("/mnt/c/Users/natha/AppData/Local/com.natkins.scribe/status.json");
        if !real.exists() {
            return;
        }
        let s = read_fallback_at(&real);
        assert!(
            !s.listening,
            "the general is not dictating -> not listening"
        );
        assert!(
            s.status.is_some(),
            "the real file's status field was parsed"
        );
    }
}
