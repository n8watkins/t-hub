//! Shared wire types + formatting helpers for the sidebar overlays (T9).
//!
//! Everything here is plain data, mirroring the server's serde shapes
//! (`apps/desktop/src-tauri/src/model.rs`, `claude/status.rs`, `agent/emit.rs`).
//! Field casing is load-bearing: the control socket speaks camelCase for these
//! payloads (host_metrics is the snake_case exception, see `metrics.rs`).

use serde::Deserialize;

/// FR-012 session status (`model.rs SessionStatus`, camelCase on the wire).
/// `#[serde(other)]` folds a future unknown variant into `Unknown` instead of
/// failing the whole payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Working,
    WaitingOnSubagents,
    NeedsQuestion,
    NeedsPermission,
    Completed,
    Failed,
    RateLimited,
    Detached,
    Restoring,
    Expired,
    #[default]
    #[serde(other)]
    Unknown,
}

impl SessionStatus {
    /// Short human label for badges/rows.
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Working => "working",
            SessionStatus::WaitingOnSubagents => "waiting on subagents",
            SessionStatus::NeedsQuestion => "needs answer",
            SessionStatus::NeedsPermission => "needs permission",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::RateLimited => "rate limited",
            SessionStatus::Detached => "detached",
            SessionStatus::Restoring => "restoring",
            SessionStatus::Expired => "expired",
            SessionStatus::Unknown => "-",
        }
    }

    /// Badge color as plain RGB (gpui-free; the render fn converts).
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            SessionStatus::Working => (96, 165, 250),             // blue
            SessionStatus::WaitingOnSubagents => (192, 132, 252), // purple
            SessionStatus::NeedsQuestion | SessionStatus::NeedsPermission => (251, 191, 36), // amber
            SessionStatus::Completed => (52, 211, 153),           // green
            SessionStatus::Failed | SessionStatus::RateLimited => (248, 113, 113), // red
            SessionStatus::Restoring => (103, 232, 249),          // cyan
            SessionStatus::Detached | SessionStatus::Expired => (115, 115, 115), // gray
            SessionStatus::Unknown => (163, 163, 163),            // neutral
        }
    }
}

/// One rate-limit window off a statusline snapshot (`claude/status.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    #[serde(default)]
    pub resets_at: Option<i64>, // unix epoch SECONDS
    #[serde(default)]
    pub used_percentage: Option<f32>, // 0..=100
}

/// A `status://snapshot` payload (`claude/status.rs StatusSnapshot`, camelCase).
/// Every field is optional so absent blocks degrade gracefully.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshot {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tmux_pane: Option<String>,
    #[serde(default)]
    pub tmux_session: Option<String>,
    #[serde(default)]
    pub context_used_pct: Option<f32>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub five_hour: Option<RateLimitWindow>,
    #[serde(default)]
    pub seven_day: Option<RateLimitWindow>,
    #[serde(default)]
    pub rate_limits_present: bool,
    #[serde(default)]
    pub ingested_at_ms: u64,
}

/// A `session://status` payload (`agent/emit.rs SessionStatusPayload`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusEvent {
    pub session_id: String,
    pub status: SessionStatus,
}

/// An `agent://title` payload (`agent/emit.rs SessionTitlePayload`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleEvent {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    pub title: String,
}

/// Wall clock in unix-epoch milliseconds. Reducers take `now_ms` as a parameter
/// so they stay deterministic under test; this is the production time source the
/// feed and render paths pass in.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compact "how long ago" label, mirroring the webview's RecentList buckets:
/// "now", "3m", "2h", "5d", "3mo".
pub fn rel_time(now_ms: u64, then_secs: i64) -> String {
    let then_ms = then_secs.saturating_mul(1000).max(0) as u64;
    let diff_s = now_ms.saturating_sub(then_ms) / 1000;
    if diff_s < 60 {
        "now".to_string()
    } else if diff_s < 3600 {
        format!("{}m", diff_s / 60)
    } else if diff_s < 86_400 {
        format!("{}h", diff_s / 3600)
    } else if diff_s < 30 * 86_400 {
        format!("{}d", diff_s / 86_400)
    } else {
        format!("{}mo", diff_s / (30 * 86_400))
    }
}

/// Compact "until" label for reset hints: "now", "in 5m", "in 2h 15m", "in 3d".
/// Used instead of the webview's absolute locale strings - epoch-to-local-time
/// needs a timezone database the native crate does not carry (documented §5).
pub fn fmt_eta(now_secs: i64, at_secs: i64) -> String {
    let diff = at_secs - now_secs;
    if diff <= 0 {
        return "now".to_string();
    }
    let (d, h, m) = (diff / 86_400, (diff % 86_400) / 3600, (diff % 3600) / 60);
    if d > 0 {
        format!("in {d}d")
    } else if h > 0 && m > 0 {
        format!("in {h}h {m}m")
    } else if h > 0 {
        format!("in {h}h")
    } else {
        format!("in {}m", m.max(1))
    }
}

/// Compact duration label for finished subagents: "800ms", "42s", "3m10s", "2h4m".
pub fn fmt_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        let (m, r) = (s / 60, s % 60);
        if r == 0 { format!("{m}m") } else { format!("{m}m{r}s") }
    } else {
        let (h, r) = (s / 3600, (s % 3600) / 60);
        if r == 0 { format!("{h}h") } else { format!("{h}h{r}m") }
    }
}

/// Last path segment (the webview's `cwdBasename`).
pub fn cwd_basename(cwd: &str) -> &str {
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_parses_camel_case_and_folds_unknown() {
        let s: SessionStatus = serde_json::from_str("\"waitingOnSubagents\"").unwrap();
        assert_eq!(s, SessionStatus::WaitingOnSubagents);
        let s: SessionStatus = serde_json::from_str("\"needsPermission\"").unwrap();
        assert_eq!(s, SessionStatus::NeedsPermission);
        // A future server variant must not fail the payload.
        let s: SessionStatus = serde_json::from_str("\"somethingNew\"").unwrap();
        assert_eq!(s, SessionStatus::Unknown);
    }

    #[test]
    fn status_snapshot_parses_the_wire_shape() {
        let v = serde_json::json!({
            "sessionId": "abc-123",
            "tmuxSession": "th_deadbeef",
            "contextUsedPct": 41.5,
            "costUsd": 1.25,
            "fiveHour": {"resetsAt": 1750000000, "usedPercentage": 62.0},
            "rateLimitsPresent": true,
            "ingestedAtMs": 1750000000000u64
        });
        let s: StatusSnapshot = serde_json::from_value(v).unwrap();
        assert_eq!(s.session_id, "abc-123");
        assert_eq!(s.tmux_session.as_deref(), Some("th_deadbeef"));
        assert_eq!(s.five_hour.unwrap().used_percentage, Some(62.0));
        assert!(s.seven_day.is_none());
        assert!(s.rate_limits_present);
    }

    #[test]
    fn rel_time_buckets() {
        let now: u64 = 1_750_000_000_000;
        let at = |secs_ago: i64| (now as i64 / 1000) - secs_ago;
        assert_eq!(rel_time(now, at(5)), "now");
        assert_eq!(rel_time(now, at(3 * 60)), "3m");
        assert_eq!(rel_time(now, at(2 * 3600)), "2h");
        assert_eq!(rel_time(now, at(5 * 86_400)), "5d");
        assert_eq!(rel_time(now, at(90 * 86_400)), "3mo");
    }

    #[test]
    fn fmt_eta_buckets() {
        let now = 1_750_000_000i64;
        assert_eq!(fmt_eta(now, now - 5), "now");
        assert_eq!(fmt_eta(now, now + 30), "in 1m");
        assert_eq!(fmt_eta(now, now + 5 * 60), "in 5m");
        assert_eq!(fmt_eta(now, now + 2 * 3600 + 15 * 60), "in 2h 15m");
        assert_eq!(fmt_eta(now, now + 3 * 86_400), "in 3d");
    }

    #[test]
    fn fmt_duration_buckets() {
        assert_eq!(fmt_duration(800), "800ms");
        assert_eq!(fmt_duration(42_000), "42s");
        assert_eq!(fmt_duration(190_000), "3m10s");
        assert_eq!(fmt_duration(2 * 3_600_000 + 4 * 60_000), "2h4m");
    }

    #[test]
    fn cwd_basename_handles_trailing_slash() {
        assert_eq!(cwd_basename("/home/n/projects/t-hub"), "t-hub");
        assert_eq!(cwd_basename("/home/n/projects/t-hub/"), "t-hub");
    }
}
