//! The status bridge (PLAN.md Workstream B): ingest Claude's statusline JSON and
//! keep the latest snapshot per **exact session id**.
//!
//! ## Verified statusline fields (REVIEW)
//!   - `context_window.*` usage,
//!   - `cost.*`,
//!   - `rate_limits.five_hour.resets_at` / `seven_day.resets_at` (Unix epoch) +
//!     `*.used_percentage`.
//!
//! ## The hard caveat (REVIEW / PRD §6.10)
//! The `rate_limits` block exists **only for Claude.ai Pro/Max** and **only
//! after the session's first API response**. So a snapshot may legitimately have
//! `rate_limits == None`; consumers must treat reset time as initially unknown
//! and degrade gracefully. Non-worktree git branch is **not** in the statusline
//! — derive it via the agent's `git branch --show-current` (see
//! [`crate::agent::AgentBridge::git_branch`]).
//!
//! This module is fully implemented (parse-tolerant ingestion + per-session
//! store); it is self-contained and feeds [`crate::model::AgentSessionRecord`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Hard cap on live per-session snapshots kept in [`StatusBridge::latest`]. Like
/// the supervision map, this grows one entry per Claude session id (a fresh UUID
/// per spawn/resume) and — because the statusline never carries a session-end
/// signal (see [`StatusBridge::ingest`]) — nothing organically removes ended
/// sessions. So we hard-bound the map: once it exceeds this many entries, the
/// least-recently-ingested snapshot is evicted (LRU by a monotonic touch stamp, not
/// wall-clock). 256 covers every realistically-concurrent session while keeping the
/// store from leaking over a long-lived hub.
const STATUS_MAP_CAP: usize = 256;

/// One rate-limit window from the statusline `rate_limits` block. Both fields
/// are optional because the block may be partial.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    /// Unix-epoch seconds the window resets (None until known — Pro/Max +
    /// after-first-response caveat).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// Percentage of the window used (0..=100), when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<f32>,
}

/// A normalized snapshot of one statusline JSON payload, keyed by exact session
/// id. Every field is optional so absent blocks degrade gracefully.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshot {
    /// The exact session id this snapshot is for.
    pub session_id: String,
    /// The session's working directory, lifted straight from the statusline
    /// payload's `cwd`. Carried so the per-tile context meter has a FALLBACK
    /// correlation when the robust tmux binding below is unavailable (e.g. an
    /// un-upgraded agent that doesn't stamp the pane). Absent when the statusline
    /// omitted it. See `store/sessionContext.ts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The tmux PANE id (`$TMUX_PANE`, e.g. `%37`) the statusline ran inside, as
    /// stamped by `t-hub-agent --statusline`. Diagnostic / future-proofing; the
    /// frontend binds on `tmux_session` below (which it can compute for a tile),
    /// but the pane id is the underlying robust signal the agent reads. Absent
    /// when not under tmux (or an un-upgraded agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    /// The tmux SESSION NAME that owns the pane the statusline ran inside (e.g.
    /// `th_<terminalId>`), resolved by the agent from `$TMUX_PANE`. This is the
    /// ROBUST tile↔session key: T-Hub names every session `th_<terminalId>`, so
    /// a tile computes its own session name and looks itself up by it — no cwd
    /// guessing. Absent ⇒ frontend degrades to the `cwd` match above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    /// Context window used %, derived from `context_window.*` (0..=100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_used_pct: Option<f32>,
    /// Total cost so far (`cost.total_cost_usd` or similar), when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The 5-hour rate-limit window (None when the block is absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<RateLimitWindow>,
    /// The 7-day rate-limit window (None when the block is absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<RateLimitWindow>,
    /// Whether the `rate_limits` block was present at all (false ⇒ free tier or
    /// pre-first-response; treat reset time as unknown).
    pub rate_limits_present: bool,
    /// Epoch-ms the snapshot was ingested (core clock).
    pub ingested_at_ms: u64,
}

impl StatusSnapshot {
    /// Parse a raw statusline JSON object into a normalized snapshot for
    /// `session_id`. Tolerant of missing fields/blocks (returns a snapshot with
    /// `None`s rather than failing). `now_ms` is injected for testability.
    pub fn from_statusline(session_id: &str, raw: &serde_json::Value, now_ms: u64) -> Self {
        // Statusline `cwd` is a top-level string; kept verbatim as the FALLBACK
        // correlation when the tmux binding below is absent.
        let cwd = raw
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // Robust tile binding: the agent stamps the owning tmux pane + session
        // (`tmux_pane`/`tmux_session`) onto the statusline before journaling. Lift
        // both verbatim; the frontend keys context by `tmux_session` and falls
        // back to `cwd` when these are absent (un-upgraded agent / not under tmux).
        let tmux_pane = raw
            .get("tmux_pane")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let tmux_session = raw
            .get("tmux_session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let context_used_pct = context_used_pct(raw);
        let cost_usd = raw
            .get("cost")
            .and_then(|c| c.get("total_cost_usd").or_else(|| c.get("total_cost")))
            .and_then(|v| v.as_f64());

        let rl = raw.get("rate_limits");
        let rate_limits_present = rl.map(|v| v.is_object()).unwrap_or(false);
        let five_hour = rl.and_then(|v| v.get("five_hour")).map(parse_window);
        let seven_day = rl.and_then(|v| v.get("seven_day")).map(parse_window);

        Self {
            session_id: session_id.to_string(),
            cwd,
            tmux_pane,
            tmux_session,
            context_used_pct,
            cost_usd,
            five_hour,
            seven_day,
            rate_limits_present,
            ingested_at_ms: now_ms,
        }
    }

    /// True when usage in either window is at/over `threshold` percent — the
    /// `rate-limited` status precondition. False when the block is absent.
    pub fn near_limit(&self, threshold: f32) -> bool {
        let over = |w: &Option<RateLimitWindow>| {
            w.as_ref()
                .and_then(|w| w.used_percentage)
                .map(|p| p >= threshold)
                .unwrap_or(false)
        };
        over(&self.five_hour) || over(&self.seven_day)
    }
}

/// Derive context-used percentage from a `context_window` block. Supports either
/// a direct `used_percentage` or a `used`/`total` pair.
fn context_used_pct(raw: &serde_json::Value) -> Option<f32> {
    let cw = raw.get("context_window")?;
    if let Some(p) = cw.get("used_percentage").and_then(|v| v.as_f64()) {
        return Some(p as f32);
    }
    let used = cw.get("used").and_then(|v| v.as_f64())?;
    let total = cw.get("total").and_then(|v| v.as_f64())?;
    if total > 0.0 {
        Some(((used / total) * 100.0) as f32)
    } else {
        None
    }
}

fn parse_window(v: &serde_json::Value) -> RateLimitWindow {
    RateLimitWindow {
        resets_at: v.get("resets_at").and_then(|x| x.as_i64()),
        used_percentage: v
            .get("used_percentage")
            .and_then(|x| x.as_f64())
            .map(|p| p as f32),
    }
}

/// The latest-snapshot-per-session store. Thread-safe; the status-ingest path
/// (from the journal `StatusSnapshot` event or a direct bridge call) writes,
/// and the Tauri status commands read.
///
/// ## Native session-restore hook (WS-6)
/// Every ingested snapshot is the freshest proof of "this Claude session is
/// running, here, in this tile". The status-ingest path is therefore the single,
/// correct place to durably record the tile→session binding the boot-time restore
/// catalog reads back: BOTH ingest paths (the journal `StatusSnapshot` entry AND
/// the `ingest_status` command) funnel through [`StatusBridge::ingest`], so hooking
/// it here captures every session with one integration point. We need all three of
/// `session_id`, `tmux_session` (the `th_<terminalId>` ⇒ the tile id), and `cwd`
/// to write a usable row; a snapshot missing any of them (un-upgraded agent / not
/// under tmux) is still stored for the usage meter but not recorded for restore.
#[derive(Default)]
pub struct StatusBridge {
    /// Latest snapshot per session, paired with a monotonic `touch` stamp (from
    /// [`Self::touch_seq`]) used purely as an LRU key for the [`STATUS_MAP_CAP`]
    /// backstop. The stamp is NOT wall-clock, so eviction order is deterministic
    /// and test-stable even though snapshots also carry an `ingested_at_ms` clock.
    latest: RwLock<HashMap<String, (u64, StatusSnapshot)>>,
    /// Monotonic counter bumped on every ingest to stamp `latest` entries with
    /// their recency for the LRU cap. Atomic so `ingest` keeps its `&self` shape.
    touch_seq: AtomicU64,
    /// The durable DB, wired in `setup()` after the AppHandle exists (the bridge
    /// is built in `AppState::default()`, before any DB). `None` until then (and
    /// in tests), in which case the WS-6 record is silently skipped.
    db: RwLock<Option<std::sync::Arc<crate::db::Db>>>,
    /// Dedup cache for the WS-6 restore record (#8): the last (session_id, cwd) we
    /// actually wrote for each `terminal_id`. The tile→session row is write-once-
    /// per-session, but a statusline ingests on EVERY refresh; so we skip the
    /// SQLite upsert when the tuple is unchanged from what's cached here. A miss /
    /// changed tuple writes the row and refreshes the cache.
    last_recorded: Mutex<HashMap<String, (String, String)>>,
}

impl StatusBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the durable DB so ingested snapshots also record the per-tile session
    /// binding for native restore (WS-6). Called once from `setup()` alongside the
    /// emitter wiring; before this the restore-record is a no-op.
    pub fn set_db(&self, db: std::sync::Arc<crate::db::Db>) {
        *self.db.write() = Some(db);
    }

    /// Ingest a raw statusline payload for `session_id`, storing the normalized
    /// snapshot and returning it.
    pub fn ingest(
        &self,
        session_id: &str,
        raw: &serde_json::Value,
        now_ms: u64,
    ) -> StatusSnapshot {
        let snap = StatusSnapshot::from_statusline(session_id, raw, now_ms);
        // Bump the monotonic touch counter so this entry is the most-recently-used
        // and the cap evicts true LRU victims (never the session we just ingested).
        let touch = self.touch_seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut latest = self.latest.write();
            latest.insert(session_id.to_string(), (touch, snap.clone()));
            // Backstop for the leak: the statusline carries no session-end signal,
            // so ended sessions are never removed organically. Hard-bound the map
            // by evicting the least-recently-ingested entries once over the cap.
            Self::enforce_cap(&mut latest);
        }
        // WS-6: durably record the tile→session binding for boot-time restore.
        self.record_for_restore(&snap);
        snap
    }

    /// Bound `latest` to [`STATUS_MAP_CAP`] entries, evicting the least-recently-
    /// ingested snapshots (lowest touch stamp) until back at the cap. Called under
    /// the write lock from [`Self::ingest`]. The just-inserted entry has the highest
    /// stamp and is never the victim. Normally a no-op (one insert grows the map by
    /// at most one); the loop is robust if the cap were lowered.
    fn enforce_cap(latest: &mut HashMap<String, (u64, StatusSnapshot)>) {
        if latest.len() <= STATUS_MAP_CAP {
            return;
        }
        let mut by_recency: Vec<(u64, String)> = latest
            .iter()
            .map(|(id, (touch, _))| (*touch, id.clone()))
            .collect();
        by_recency.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let to_remove = latest.len() - STATUS_MAP_CAP;
        for (_, id) in by_recency.into_iter().take(to_remove) {
            latest.remove(&id);
        }
    }

    /// Drop a session's snapshot — the seam for an explicit session-end eviction.
    /// The statusline payloads that feed [`Self::ingest`] carry no lifecycle/end
    /// signal, so today the bridge relies on the [`STATUS_MAP_CAP`] LRU backstop to
    /// bound growth; if a `SessionEnd` signal is ever routed here (as the
    /// supervision reducer already evicts on), call this for prompt cleanup. Kept
    /// in-crate and harmless when the session is already gone.
    pub fn evict(&self, session_id: &str) {
        self.latest.write().remove(session_id);
    }

    /// Best-effort: upsert this snapshot's tile→session binding into the durable
    /// `tile_sessions` map (WS-6), keyed by the tile id (`th_<id>` ⇒ `<id>`). Only
    /// fires when the snapshot carries the robust tmux binding (`tmux_session`) AND
    /// a `cwd` — without both we can't restore the session to the right place, so
    /// we skip rather than write a half-row. A DB error is swallowed (logged by the
    /// DB layer); recording must never disturb the live usage meter.
    ///
    /// #8: the row is write-once-per-session, but a statusline ingests on every
    /// refresh — so we keep a per-tile cache of the last (session_id, cwd) written
    /// and skip the SQLite upsert entirely when the tuple is unchanged.
    fn record_for_restore(&self, snap: &StatusSnapshot) {
        let Some(db) = self.db.read().clone() else {
            return; // no DB wired (pre-setup / tests)
        };
        let (Some(tmux_session), Some(cwd)) = (snap.tmux_session.as_deref(), snap.cwd.as_deref())
        else {
            return; // un-upgraded agent / not under tmux — can't bind a tile.
        };
        if snap.session_id.is_empty() || cwd.trim().is_empty() {
            return;
        }
        // The terminal id is the tmux session name minus the `th_` prefix (T-Hub
        // names every session `th_<terminalId>`). Fall back to the whole name if
        // the prefix is somehow absent so the row is still keyed consistently.
        let terminal_id = tmux_session.strip_prefix("th_").unwrap_or(tmux_session);
        // #8: skip the upsert when this tile's (session_id, cwd) is unchanged from
        // what we last wrote — the common case on a repeating statusline. We update
        // the cache only AFTER recording so a transient DB error is retried next
        // ingest rather than masked by a premature cache write.
        {
            let cache = self.last_recorded.lock().expect("status dedup mutex poisoned");
            if cache
                .get(terminal_id)
                .is_some_and(|(s, c)| s == &snap.session_id && c == cwd)
            {
                return;
            }
        }
        if db
            .record_tile_session(terminal_id, &snap.session_id, cwd, tmux_session)
            .is_ok()
        {
            self.last_recorded
                .lock()
                .expect("status dedup mutex poisoned")
                .insert(terminal_id.to_string(), (snap.session_id.clone(), cwd.to_string()));
        }
    }

    /// The latest snapshot for a session, if any.
    pub fn get(&self, session_id: &str) -> Option<StatusSnapshot> {
        self.latest.read().get(session_id).map(|(_, snap)| snap.clone())
    }

    /// All known snapshots (for the utility-area usage display).
    pub fn all(&self) -> Vec<StatusSnapshot> {
        self.latest.read().values().map(|(_, snap)| snap.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingests_full_promax_payload() {
        let raw = serde_json::json!({
            "cwd": "/home/u/proj",
            "context_window": { "used_percentage": 42.5 },
            "cost": { "total_cost_usd": 1.23 },
            "rate_limits": {
                "five_hour": { "resets_at": 1_700_000_000, "used_percentage": 80.0 },
                "seven_day": { "resets_at": 1_700_500_000, "used_percentage": 10.0 }
            }
        });
        let snap = StatusSnapshot::from_statusline("s1", &raw, 999);
        assert_eq!(snap.cwd.as_deref(), Some("/home/u/proj"));
        // No tmux binding in this payload (un-upgraded agent / not under tmux).
        assert!(snap.tmux_pane.is_none());
        assert!(snap.tmux_session.is_none());
        assert_eq!(snap.context_used_pct, Some(42.5));
        assert_eq!(snap.cost_usd, Some(1.23));
        assert!(snap.rate_limits_present);
        assert_eq!(snap.five_hour.as_ref().unwrap().resets_at, Some(1_700_000_000));
        assert!(snap.near_limit(75.0));
        assert!(!snap.near_limit(90.0));
    }

    #[test]
    fn degrades_gracefully_without_rate_limits_block() {
        // Free tier / pre-first-response: no rate_limits block at all.
        let raw = serde_json::json!({
            "context_window": { "used": 50000, "total": 200000 }
        });
        let snap = StatusSnapshot::from_statusline("s1", &raw, 1);
        assert!(snap.cwd.is_none(), "absent cwd must stay None");
        assert!(!snap.rate_limits_present);
        assert!(snap.five_hour.is_none());
        assert!(snap.seven_day.is_none());
        assert!(!snap.near_limit(50.0), "absent block must not read as near-limit");
        // context derived from used/total.
        assert_eq!(snap.context_used_pct, Some(25.0));
    }

    #[test]
    fn lifts_tmux_pane_and_session_for_robust_binding() {
        // The agent stamps the owning tmux pane + session onto the statusline;
        // both must be lifted so the frontend can bind the snapshot to the exact
        // tile (`th_<id>`) rather than guessing by cwd.
        let raw = serde_json::json!({
            "cwd": "/work",
            "context_window": { "used_percentage": 12.0 },
            "tmux_pane": "%37",
            "tmux_session": "th_abcd1234"
        });
        let snap = StatusSnapshot::from_statusline("s1", &raw, 1);
        assert_eq!(snap.tmux_pane.as_deref(), Some("%37"));
        assert_eq!(snap.tmux_session.as_deref(), Some("th_abcd1234"));
        // cwd still carried as the fallback correlation.
        assert_eq!(snap.cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn bridge_stores_latest_per_session() {
        let bridge = StatusBridge::new();
        bridge.ingest("s1", &serde_json::json!({"context_window":{"used_percentage":10.0}}), 1);
        bridge.ingest("s1", &serde_json::json!({"context_window":{"used_percentage":20.0}}), 2);
        bridge.ingest("s2", &serde_json::json!({"context_window":{"used_percentage":5.0}}), 3);
        assert_eq!(bridge.get("s1").unwrap().context_used_pct, Some(20.0));
        assert_eq!(bridge.get("s2").unwrap().context_used_pct, Some(5.0));
        assert_eq!(bridge.all().len(), 2);
    }

    // --- Native session-restore hook (WS-6) ---------------------------------

    /// A bridge wired to a temp DB records a tile→session binding on ingest when
    /// the snapshot carries the robust tmux binding + cwd, deriving the terminal
    /// id from the `th_<id>` session name. A snapshot missing either is stored for
    /// the usage meter but NOT recorded for restore.
    #[test]
    fn ingest_records_tile_session_only_with_tmux_binding_and_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "th-status-ws6-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = std::sync::Arc::new(crate::db::Db::open_in(dir.clone()));
        let bridge = StatusBridge::new();
        bridge.set_db(db.clone());

        // Full snapshot: tmux_session + cwd present -> recorded under terminal id
        // `abcd1234` (the `th_` prefix stripped).
        bridge.ingest(
            "claude-sess-1",
            &serde_json::json!({
                "cwd": "/home/u/proj",
                "tmux_session": "th_abcd1234",
                "context_window": { "used_percentage": 10.0 }
            }),
            1,
        );
        // No tmux binding -> stored for usage but NOT recorded for restore.
        bridge.ingest(
            "claude-sess-2",
            &serde_json::json!({ "cwd": "/home/u/other", "context_window": { "used_percentage": 5.0 } }),
            2,
        );

        let rows = db.all_tile_sessions().unwrap();
        assert_eq!(rows.len(), 1, "only the tmux-bound snapshot is recorded");
        assert_eq!(rows[0].terminal_id, "abcd1234");
        assert_eq!(rows[0].session_id, "claude-sess-1");
        assert_eq!(rows[0].cwd, "/home/u/proj");
        assert_eq!(rows[0].tmux_session, "th_abcd1234");
        // Both snapshots are still queryable for the usage meter.
        assert!(bridge.get("claude-sess-1").is_some());
        assert!(bridge.get("claude-sess-2").is_some());

        drop(bridge);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// #8: a repeating statusline for the SAME (terminal, session, cwd) is
    /// deduped — only the first ingest writes the row; a CHANGED session/cwd
    /// breaks the cache and writes again (the row still upserts in place).
    #[test]
    fn record_for_restore_dedups_unchanged_tuple() {
        let dir = std::env::temp_dir().join(format!(
            "th-status-ws6-dedup-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = std::sync::Arc::new(crate::db::Db::open_in(dir.clone()));
        let bridge = StatusBridge::new();
        bridge.set_db(db.clone());

        let payload = serde_json::json!({
            "cwd": "/home/u/proj",
            "tmux_session": "th_t1",
            "context_window": { "used_percentage": 10.0 }
        });
        // Three identical ingests: the dedup cache caches after the first, so the
        // tuple is recorded once and skipped twice. Observable via the cache state.
        bridge.ingest("sess-a", &payload, 1);
        bridge.ingest("sess-a", &payload, 2);
        bridge.ingest("sess-a", &payload, 3);
        {
            let cache = bridge.last_recorded.lock().unwrap();
            assert_eq!(
                cache.get("t1"),
                Some(&("sess-a".to_string(), "/home/u/proj".to_string())),
                "cache holds the last-written tuple after dedup",
            );
        }
        // A new session for the same tile is a cache miss -> records again.
        bridge.ingest(
            "sess-b",
            &serde_json::json!({
                "cwd": "/home/u/proj2",
                "tmux_session": "th_t1",
                "context_window": { "used_percentage": 20.0 }
            }),
            4,
        );
        let rows = db.all_tile_sessions().unwrap();
        assert_eq!(rows.len(), 1, "still one row per terminal_id (upsert)");
        assert_eq!(rows[0].session_id, "sess-b", "the changed session was written");
        assert_eq!(rows[0].cwd, "/home/u/proj2");
        {
            let cache = bridge.last_recorded.lock().unwrap();
            assert_eq!(
                cache.get("t1"),
                Some(&("sess-b".to_string(), "/home/u/proj2".to_string())),
                "cache refreshed to the new tuple",
            );
        }

        drop(bridge);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With no DB wired (the pre-setup / default state), ingest still stores the
    /// snapshot and the restore-record is a silent no-op.
    #[test]
    fn ingest_without_db_skips_restore_record() {
        let bridge = StatusBridge::new();
        let snap = bridge.ingest(
            "s1",
            &serde_json::json!({ "cwd": "/c", "tmux_session": "th_t1" }),
            1,
        );
        assert_eq!(snap.session_id, "s1");
        assert!(bridge.get("s1").is_some());
    }

    // --- Memory-leak fix: bounded growth ------------------------------------

    fn ctx_payload(pct: f64) -> serde_json::Value {
        serde_json::json!({ "context_window": { "used_percentage": pct } })
    }

    /// The statusline never carries a session-end signal, so `latest` would grow
    /// one entry per session id forever. The cap backstop hard-bounds it: once over
    /// the cap, the least-recently-ingested snapshot is evicted (LRU, recency-based
    /// not insertion-based).
    #[test]
    fn latest_map_is_capped_and_evicts_least_recently_ingested() {
        let bridge = StatusBridge::new();
        // Fill exactly to the cap, one ingest each in ascending order.
        for i in 0..STATUS_MAP_CAP {
            bridge.ingest(&format!("s{i:05}"), &ctx_payload(i as f64 % 100.0), i as u64);
        }
        assert_eq!(bridge.all().len(), STATUS_MAP_CAP, "filled to the cap");

        // Re-ingest the oldest session so it's no longer the LRU victim.
        bridge.ingest("s00000", &ctx_payload(99.0), 9_000);

        // One brand-new session pushes over the cap → exactly one eviction.
        bridge.ingest("s99999", &ctx_payload(1.0), 9_001);
        assert_eq!(bridge.all().len(), STATUS_MAP_CAP, "map stays bounded at the cap");

        // The re-ingested oldest survives; the now-least-recent (s00001) is evicted.
        assert!(bridge.get("s00000").is_some(), "re-ingested session survives");
        assert!(bridge.get("s00001").is_none(), "least-recently-ingested evicted");
        assert!(bridge.get("s99999").is_some(), "the new session is present");
    }

    /// The explicit eviction seam drops a session's snapshot (used if a session-end
    /// signal is ever routed to the bridge) and is harmless when already gone.
    #[test]
    fn evict_removes_snapshot_and_is_idempotent() {
        let bridge = StatusBridge::new();
        bridge.ingest("s1", &ctx_payload(10.0), 1);
        assert!(bridge.get("s1").is_some());
        bridge.evict("s1");
        assert!(bridge.get("s1").is_none(), "evicted snapshot is gone");
        // Evicting an unknown / already-evicted session is a harmless no-op.
        bridge.evict("s1");
        bridge.evict("never-seen");
        assert!(bridge.all().is_empty());
    }
}
