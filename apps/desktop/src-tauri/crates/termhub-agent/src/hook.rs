//! `--hook <EVENT>` ingest mode: Claude Code invokes the installed handler
//! script which execs `termhub-agent --hook <EVENT>`. This module reads the
//! hook's JSON payload from stdin, builds a durable [`EventJournalEntry`], and
//! appends it to the journal — closing the hook→journal half of the event spine.
//!
//! ## Contract
//! - ALWAYS exits 0.  A hook handler must never fail Claude's turn.
//! - Any error (parse failure, journal open failure, append failure) is logged
//!   to stderr only; the process still exits 0.
//! - Fast: one read, one journal append, one fsync, done.

use std::io::Read;
use std::path::PathBuf;

use termhub_protocol::{EventJournalEntry, JournalEventType, JournalSource};

/// Map a Claude Code hook event name to the corresponding [`JournalEventType`].
/// Returns `JournalEventType::Unknown` for an unrecognised name so future hooks
/// are journaled without crashing older agents.
///
/// The 15 names below are the verified set from REVIEW.md §9.6 and must exactly
/// match the names in `src/claude/hooks.rs::event_type_for_hook` on the core
/// side (the agent can't import the core crate, so we maintain an identical
/// local copy).
pub fn event_type_for_hook(hook_name: &str) -> JournalEventType {
    use JournalEventType::*;
    match hook_name {
        "SessionStart"      => SessionStart,
        "SessionEnd"        => SessionEnd,
        "UserPromptSubmit"  => UserPromptSubmit,
        "Stop"              => Stop,
        "StopFailure"       => StopFailure,
        "PermissionRequest" => PermissionRequest,
        "Notification"      => Notification,
        "Elicitation"       => Elicitation,
        "SubagentStart"     => SubagentStart,
        "SubagentStop"      => SubagentStop,
        "TaskCreated"       => TaskCreated,
        "TaskCompleted"     => TaskCompleted,
        "CwdChanged"        => CwdChanged,
        "WorktreeCreate"    => WorktreeCreate,
        "WorktreeRemove"    => WorktreeRemove,
        _                   => Unknown,
    }
}

/// Return the current time as Unix epoch milliseconds (agent clock).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build a journal entry from a hook event name and its parsed JSON payload.
/// This is a pure function so it is easily unit-tested without any I/O.
///
/// Fields set:
/// - `seq`: 0 (assigned by [`Journal::append`])
/// - `timestamp_ms`: now (epoch-ms)
/// - `source`: [`JournalSource::Hook`]
/// - `event_type`: mapped from `hook_name` (Unknown for unrecognised names)
/// - `entity_id`: the `session_id` string from `payload`, if present
/// - `payload`: the full JSON value (preserves agent_id, agent_type, cwd, etc.)
/// - `result`: None
pub fn build_entry(hook_name: &str, payload: &serde_json::Value) -> EventJournalEntry {
    let event_type = event_type_for_hook(hook_name);
    let entity_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    EventJournalEntry {
        seq: 0,
        timestamp_ms: now_ms(),
        source: JournalSource::Hook,
        entity_id,
        event_type,
        payload: payload.clone(),
        result: None,
    }
}

/// Run the `--hook <EVENT>` ingest path.
///
/// 1. Read hook JSON from stdin to EOF.
/// 2. Parse as `serde_json::Value` (tolerate failure — journal an empty object).
/// 3. Build and append a journal entry.
/// 4. Exit 0 always (never block Claude).
///
/// Any error is written to stderr; the process still returns normally so the
/// caller can exit 0.
pub fn run(hook_name: &str, journal_dir: Option<&str>) -> anyhow::Result<()> {
    // 1. Read stdin to EOF.
    let mut raw = String::new();
    {
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        if let Err(e) = handle.read_to_string(&mut raw) {
            eprintln!("termhub-agent --hook {hook_name}: failed reading stdin: {e:#}");
            // raw stays empty; we continue with a null payload.
        }
    }

    // 2. Parse JSON (tolerate failure).
    let payload: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "termhub-agent --hook {hook_name}: failed parsing hook JSON: {e:#}; \
                 journaling with empty payload"
            );
            serde_json::Value::Null
        }
    };

    // 3. Build the entry.
    let entry = build_entry(hook_name, &payload);

    // 4. Open journal and append.
    let dir: PathBuf = crate::journal::resolve_journal_dir(journal_dir);
    let journal = match crate::journal::Journal::open(&dir) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "termhub-agent --hook {hook_name}: failed to open journal at {dir:?}: {e:#}"
            );
            // Return Ok so main exits 0.
            return Ok(());
        }
    };
    if let Err(e) = journal.append(entry) {
        eprintln!("termhub-agent --hook {hook_name}: failed to append journal entry: {e:#}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Statusline ingest (`--statusline`)
// ---------------------------------------------------------------------------
//
// Claude Code's `statusLine` setting runs a command on every status refresh and
// pipes a JSON object to its stdin (session_id, cwd, model, cost, context_window,
// rate_limits, ...). We journal that raw payload as a `StatusSnapshot` entry; the
// core's `consume_journal_entry` routes `StatusSnapshot` entries through the
// status bridge and emits `status://snapshot`, which the sidebar USAGE strip
// reads. This is the missing data source that left USAGE showing dashes: hooks
// were installed but no statusline ever fed the bridge.
//
// The entry payload wraps the raw statusline under `status` and lifts
// `session_id` to the top so it matches the core's ingest contract (see
// `AgentBridge::ingest_status_from_journal`, which reads `payload.status` and
// `entity_id`/`payload.session_id`).

/// Resolve the tmux pane + session this statusline process is running inside, so
/// the core/frontend can bind a STATUS SNAPSHOT to the exact terminal tile that
/// owns the pane — a ROBUST replacement for the fragile cwd correlation (two
/// tiles in the same directory are indistinguishable by cwd; their tmux session
/// names are not).
///
/// Claude runs its statusline command INSIDE the tile's tmux pane, so tmux sets
/// `$TMUX_PANE` (e.g. `%37`) in our environment. From that pane id we ask tmux
/// (the CURRENT server via `$TMUX`, NOT a hardcoded `-L` socket — so it resolves
/// on production `termhub` AND a side-by-side dev `termhub-dev`) for the owning
/// `#{session_name}` — TermHub
/// names every session `th_<terminalId>`, which the frontend can compute for a
/// tile directly and key context by. Returns `(pane, session)`:
///   - `pane`:    the raw `$TMUX_PANE` value, or `None` if unset (not under tmux).
///   - `session`: the resolved session name, or `None` if tmux can't resolve it
///     (server gone, pane vanished) — the frontend then degrades to cwd matching.
///
/// Best-effort and side-effect-free on failure: a missing tmux / unset env / a
/// non-zero exit all collapse to `None` so a statusline render is never blocked.
fn resolve_tmux_pane() -> (Option<String>, Option<String>) {
    // `$TMUX_PANE` is the pane the statusline runs in (set by tmux for any process
    // it spawned). Absent ⇒ not running under tmux; nothing to resolve.
    let pane = match std::env::var("TMUX_PANE") {
        Ok(p) if !p.trim().is_empty() => p,
        _ => return (None, None),
    };

    // Resolve the owning session NAME from the pane id. NO `-L <socket>`: running
    // inside the pane, tmux uses `$TMUX` to reach the CURRENT server, so this
    // resolves on ANY socket (production `termhub` AND a dev `termhub-dev`).
    // `-t <pane>` targets that exact pane; `-p` prints the formatted value.
    let session = std::process::Command::new("tmux")
        .args(["display", "-p", "-t", &pane, "#{session_name}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    (Some(pane), session)
}

/// Build a `StatusSnapshot` journal entry from a parsed statusline JSON payload.
/// Pure (no I/O) so it is unit-tested directly. `entity_id` + `payload.session_id`
/// are the statusline's `session_id`; the raw statusline rides under
/// `payload.status` for the core's status bridge to parse.
pub fn build_status_entry(statusline: &serde_json::Value) -> EventJournalEntry {
    let session_id = statusline
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let payload = serde_json::json!({
        "session_id": session_id,
        "status": statusline.clone(),
    });

    EventJournalEntry {
        seq: 0,
        timestamp_ms: now_ms(),
        source: JournalSource::Status,
        entity_id: session_id,
        event_type: JournalEventType::StatusSnapshot,
        payload,
        result: None,
    }
}

/// A compact one-line readout for the actual Claude statusline (stdout), so the
/// command is a well-behaved statusline AND our journal ingest happens as a side
/// effect. Best-effort string assembly; empty when nothing useful is present.
fn statusline_readout(statusline: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = statusline
        .get("model")
        .and_then(|m| m.get("display_name"))
        .and_then(|v| v.as_str())
    {
        parts.push(model.to_string());
    }
    if let Some(pct) = statusline
        .get("context_window")
        .and_then(|c| c.get("used_percentage"))
        .and_then(|v| v.as_f64())
    {
        parts.push(format!("ctx {}%", pct.round() as i64));
    }
    if let Some(cost) = statusline
        .get("cost")
        .and_then(|c| c.get("total_cost_usd"))
        .and_then(|v| v.as_f64())
    {
        parts.push(format!("${cost:.2}"));
    }
    parts.join(" · ")
}

/// Run the `--statusline` ingest path.
///
/// 1. Read statusline JSON from stdin to EOF.
/// 2. Parse as `serde_json::Value` (tolerate failure — journal nothing, print "").
/// 3. Append a `StatusSnapshot` journal entry.
/// 4. Print a one-line readout to stdout so this is a valid statusline command.
/// 5. Exit 0 always (never block Claude's statusline render).
pub fn run_statusline(journal_dir: Option<&str>) -> anyhow::Result<()> {
    // 1. Read stdin to EOF.
    let mut raw = String::new();
    {
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        if let Err(e) = handle.read_to_string(&mut raw) {
            eprintln!("termhub-agent --statusline: failed reading stdin: {e:#}");
        }
    }

    // 2. Parse JSON. On failure we have nothing to ingest; print an empty line.
    let mut statusline: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("termhub-agent --statusline: failed parsing statusline JSON: {e:#}");
            println!();
            return Ok(());
        }
    };

    // 2b. Stamp the tmux pane + session this statusline runs inside onto the
    //     payload, so the core/frontend can bind the snapshot to the EXACT tile
    //     that owns the pane (robust) instead of correlating by cwd (fragile).
    //     Only set when actually under tmux + resolvable; absent keys degrade the
    //     frontend to its cwd fallback (so an un-upgraded agent still works).
    let (tmux_pane, tmux_session) = resolve_tmux_pane();
    if statusline.is_object() {
        let obj = statusline.as_object_mut().expect("checked is_object");
        if let Some(pane) = tmux_pane {
            obj.insert("tmux_pane".into(), serde_json::Value::String(pane));
        }
        if let Some(session) = tmux_session {
            obj.insert("tmux_session".into(), serde_json::Value::String(session));
        }
    }

    // 3. Build + append the StatusSnapshot entry.
    let entry = build_status_entry(&statusline);
    let session_id = entry.entity_id.clone().unwrap_or_default();
    let dir: PathBuf = crate::journal::resolve_journal_dir(journal_dir);
    match crate::journal::Journal::open(&dir) {
        Ok(journal) => {
            if let Err(e) = journal.append(entry) {
                eprintln!("termhub-agent --statusline: failed to append journal entry: {e:#}");
            } else {
                // Diagnostic on stderr only (stdout is the statusline render): a
                // grep-able marker the orchestrator can correlate with the core
                // emitting status://snapshot for this session.
                eprintln!(
                    "termhub-agent --statusline: journaled StatusSnapshot for session {session_id}"
                );
            }
        }
        Err(e) => {
            eprintln!("termhub-agent --statusline: failed to open journal at {dir:?}: {e:#}");
        }
    }

    // 4. Print the one-line readout so this stays a valid statusline command.
    println!("{}", statusline_readout(&statusline));

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use termhub_protocol::{JournalEventType, JournalSource};

    // -----------------------------------------------------------------------
    // event_type_for_hook mapping
    // -----------------------------------------------------------------------

    #[test]
    fn all_15_hook_names_map_correctly() {
        let cases = [
            ("SessionStart",      JournalEventType::SessionStart),
            ("SessionEnd",        JournalEventType::SessionEnd),
            ("UserPromptSubmit",  JournalEventType::UserPromptSubmit),
            ("Stop",              JournalEventType::Stop),
            ("StopFailure",       JournalEventType::StopFailure),
            ("PermissionRequest", JournalEventType::PermissionRequest),
            ("Notification",      JournalEventType::Notification),
            ("Elicitation",       JournalEventType::Elicitation),
            ("SubagentStart",     JournalEventType::SubagentStart),
            ("SubagentStop",      JournalEventType::SubagentStop),
            ("TaskCreated",       JournalEventType::TaskCreated),
            ("TaskCompleted",     JournalEventType::TaskCompleted),
            ("CwdChanged",        JournalEventType::CwdChanged),
            ("WorktreeCreate",    JournalEventType::WorktreeCreate),
            ("WorktreeRemove",    JournalEventType::WorktreeRemove),
        ];
        for (name, expected) in &cases {
            assert_eq!(
                event_type_for_hook(name),
                *expected,
                "hook name '{name}' should map to {expected:?}"
            );
        }
    }

    #[test]
    fn unknown_hook_name_maps_to_unknown() {
        assert_eq!(event_type_for_hook("FutureThing"), JournalEventType::Unknown);
        assert_eq!(event_type_for_hook(""), JournalEventType::Unknown);
        assert_eq!(event_type_for_hook("sessionstart"), JournalEventType::Unknown); // case-sensitive
    }

    // -----------------------------------------------------------------------
    // build_entry — pure unit tests (no I/O)
    // -----------------------------------------------------------------------

    #[test]
    fn build_entry_session_start_populates_all_fields() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "cwd": "/x",
            "transcript_path": "/home/u/.claude/projects/x/s1.jsonl"
        });
        let entry = build_entry("SessionStart", &payload);

        assert_eq!(entry.source, JournalSource::Hook);
        assert_eq!(entry.event_type, JournalEventType::SessionStart);
        assert_eq!(entry.entity_id.as_deref(), Some("s1"));
        assert_eq!(entry.payload["cwd"], "/x");
        assert_eq!(entry.payload["session_id"], "s1");
        assert!(entry.result.is_none());
        assert_eq!(entry.seq, 0, "seq is assigned by Journal::append, not build_entry");
        assert!(entry.timestamp_ms > 0);
    }

    #[test]
    fn build_entry_unknown_event_name_maps_to_unknown() {
        let payload = serde_json::json!({"session_id": "s2"});
        let entry = build_entry("SomeFutureHook", &payload);
        assert_eq!(entry.event_type, JournalEventType::Unknown);
        assert_eq!(entry.entity_id.as_deref(), Some("s2"));
    }

    #[test]
    fn build_entry_missing_session_id_gives_none_entity_id() {
        let payload = serde_json::json!({"cwd": "/home/u"});
        let entry = build_entry("Stop", &payload);
        assert!(entry.entity_id.is_none());
        assert_eq!(entry.event_type, JournalEventType::Stop);
    }

    #[test]
    fn build_entry_null_payload_is_tolerated() {
        let entry = build_entry("Notification", &serde_json::Value::Null);
        assert_eq!(entry.event_type, JournalEventType::Notification);
        assert!(entry.entity_id.is_none());
    }

    // -----------------------------------------------------------------------
    // build_status_entry — statusline ingest (pure)
    // -----------------------------------------------------------------------

    #[test]
    fn build_status_entry_wraps_raw_statusline_and_lifts_session_id() {
        let statusline = serde_json::json!({
            "session_id": "sess-9",
            "cwd": "/work",
            "model": { "display_name": "Opus" },
            "cost": { "total_cost_usd": 1.23 },
            "context_window": { "used_percentage": 42 },
            "rate_limits": {
                "five_hour": { "used_percentage": 80.0, "resets_at": 1_700_000_000 }
            }
        });
        let entry = build_status_entry(&statusline);

        assert_eq!(entry.source, JournalSource::Status);
        assert_eq!(entry.event_type, JournalEventType::StatusSnapshot);
        assert_eq!(entry.entity_id.as_deref(), Some("sess-9"));
        // session_id lifted to the top of the payload.
        assert_eq!(entry.payload["session_id"], "sess-9");
        // Raw statusline preserved under payload.status (what the core parses).
        assert_eq!(entry.payload["status"]["context_window"]["used_percentage"], 42);
        assert_eq!(entry.payload["status"]["cost"]["total_cost_usd"], 1.23);
        assert!(entry.timestamp_ms > 0);
        assert_eq!(entry.seq, 0, "seq is assigned by Journal::append");
    }

    #[test]
    fn build_status_entry_carries_tmux_pane_and_session() {
        // The statusline value as run_statusline stamps it: the resolved tmux pane
        // + session are injected as top-level keys before journaling. They must
        // survive under payload.status so the core's from_statusline can read them
        // and bind the snapshot to the owning tile.
        let statusline = serde_json::json!({
            "session_id": "sess-1",
            "cwd": "/work",
            "context_window": { "used_percentage": 33 },
            "tmux_pane": "%37",
            "tmux_session": "th_abcd1234"
        });
        let entry = build_status_entry(&statusline);
        assert_eq!(entry.payload["status"]["tmux_pane"], "%37");
        assert_eq!(entry.payload["status"]["tmux_session"], "th_abcd1234");
    }

    #[test]
    fn build_status_entry_tolerates_missing_session_id() {
        let statusline = serde_json::json!({ "cwd": "/x" });
        let entry = build_status_entry(&statusline);
        assert!(entry.entity_id.is_none());
        assert_eq!(entry.event_type, JournalEventType::StatusSnapshot);
        assert!(entry.payload["session_id"].is_null());
    }

    #[test]
    fn build_entry_preserves_subagent_fields() {
        let payload = serde_json::json!({
            "session_id": "parent-sess",
            "agent_id": "sub-agent-42",
            "agent_type": "subagent",
            "cwd": "/workspace"
        });
        let entry = build_entry("SubagentStart", &payload);
        assert_eq!(entry.event_type, JournalEventType::SubagentStart);
        assert_eq!(entry.entity_id.as_deref(), Some("parent-sess"));
        assert_eq!(entry.payload["agent_id"], "sub-agent-42");
        assert_eq!(entry.payload["agent_type"], "subagent");
    }

    // -----------------------------------------------------------------------
    // End-to-end: build_entry + Journal::append + journal reopen
    // -----------------------------------------------------------------------

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("termhub-hook-test-{tag}-{ts}"))
    }

    #[test]
    fn append_to_journal_and_replay_roundtrip() {
        let dir = temp_dir("e2e");

        // Build and append.
        let payload = serde_json::json!({
            "session_id": "demo-1",
            "cwd": "/home/natkins"
        });
        let entry = build_entry("SessionStart", &payload);

        {
            let j = crate::journal::Journal::open(&dir).unwrap();
            assert_eq!(j.head_seq(), 0);
            let stored = j.append(entry).unwrap();
            assert_eq!(stored.seq, 1);
            assert_eq!(j.head_seq(), 1);
        }

        // Reopen and verify.
        {
            let j2 = crate::journal::Journal::open(&dir).unwrap();
            assert_eq!(j2.head_seq(), 1);

            let entries = j2.replay(0).unwrap();
            assert_eq!(entries.len(), 1);
            let e = &entries[0];
            assert_eq!(e.seq, 1);
            assert_eq!(e.source, JournalSource::Hook);
            assert_eq!(e.event_type, JournalEventType::SessionStart);
            assert_eq!(e.entity_id.as_deref(), Some("demo-1"));
            assert_eq!(e.payload["cwd"], "/home/natkins");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn multiple_hook_appends_get_monotonic_seqs() {
        let dir = temp_dir("multi");
        let j = crate::journal::Journal::open(&dir).unwrap();

        let e1 = build_entry("SessionStart", &serde_json::json!({"session_id":"s1"}));
        let e2 = build_entry("UserPromptSubmit", &serde_json::json!({"session_id":"s1"}));
        let e3 = build_entry("Stop", &serde_json::json!({"session_id":"s1"}));

        let r1 = j.append(e1).unwrap();
        let r2 = j.append(e2).unwrap();
        let r3 = j.append(e3).unwrap();

        assert_eq!(r1.seq, 1);
        assert_eq!(r2.seq, 2);
        assert_eq!(r3.seq, 3);
        assert_eq!(j.head_seq(), 3);

        // Partial replay.
        let tail = j.replay(1).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].event_type, JournalEventType::UserPromptSubmit);
        assert_eq!(tail[1].event_type, JournalEventType::Stop);

        std::fs::remove_dir_all(&dir).ok();
    }
}
