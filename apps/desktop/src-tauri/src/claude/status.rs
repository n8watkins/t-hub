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

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

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
    /// stamped by `termhub-agent --statusline`. Diagnostic / future-proofing; the
    /// frontend binds on `tmux_session` below (which it can compute for a tile),
    /// but the pane id is the underlying robust signal the agent reads. Absent
    /// when not under tmux (or an un-upgraded agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    /// The tmux SESSION NAME that owns the pane the statusline ran inside (e.g.
    /// `th_<terminalId>`), resolved by the agent from `$TMUX_PANE`. This is the
    /// ROBUST tile↔session key: TermHub names every session `th_<terminalId>`, so
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
#[derive(Default)]
pub struct StatusBridge {
    latest: RwLock<HashMap<String, StatusSnapshot>>,
}

impl StatusBridge {
    pub fn new() -> Self {
        Self::default()
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
        self.latest
            .write()
            .insert(session_id.to_string(), snap.clone());
        snap
    }

    /// The latest snapshot for a session, if any.
    pub fn get(&self, session_id: &str) -> Option<StatusSnapshot> {
        self.latest.read().get(session_id).cloned()
    }

    /// All known snapshots (for the utility-area usage display).
    pub fn all(&self) -> Vec<StatusSnapshot> {
        self.latest.read().values().cloned().collect()
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
}
