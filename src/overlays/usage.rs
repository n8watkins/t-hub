//! Usage overlay (T9): Claude + Codex plan-usage meters and running cost.
//!
//! Source priority mirrors the webview's `UsageStrip.tsx`:
//!  - **Claude, primary:** live statusline rate limits off `status://snapshot`
//!    events (zero-cost, per-session; freshest non-null value per window wins).
//!  - **Claude, fallback:** the `claude_usage` command (runs `claude -p /usage`
//!    server-side - expensive, so only while no statusline data exists; the feed
//!    owns the cadence via [`super::feed::PollPlan`]).
//!  - **Codex:** the `codex_usage` command, with local window rollover so a
//!    stale read shows "available" once its reset passes without a re-poll.

use std::collections::HashMap;

use serde::Deserialize;

use super::model::{fmt_eta, RateLimitWindow, StatusSnapshot};

/// Parsed `claude_usage` result (`usage.rs ClaudeUsage`, camelCase). Percentages
/// are the USED amount; the UI shows "left" = 100 - used.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsage {
    #[serde(default)]
    pub session_used_pct: Option<f32>,
    #[serde(default)]
    pub session_resets: Option<String>,
    #[serde(default)]
    pub week_used_pct: Option<f32>,
    #[serde(default)]
    pub week_resets: Option<String>,
    #[serde(default)]
    pub week_sonnet_used_pct: Option<f32>,
    #[serde(default)]
    pub ok: bool,
}

/// One Codex rate window (`codex.rs CodexRateWindow`, camelCase).
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateWindow {
    #[serde(default)]
    pub used_percent: Option<f32>,
    #[serde(default)]
    pub window_minutes: Option<i64>,
    #[serde(default)]
    pub resets_at: Option<i64>, // unix epoch seconds
}

/// Parsed `codex_usage` result (`codex.rs CodexUsage`, camelCase).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    #[serde(default)]
    pub primary: Option<CodexRateWindow>, // ~5h window
    #[serde(default)]
    pub secondary: Option<CodexRateWindow>, // ~weekly window
    #[serde(default)]
    pub plan_type: Option<String>,
    #[serde(default)]
    pub ok: bool,
}

/// The freshest statusline reading for one rate window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowReading {
    pub used_pct: f32,
    pub resets_at: Option<i64>,
    pub ingested_at_ms: u64,
}

/// Per-session statusline extract kept for the meters (windows + cost).
#[derive(Debug, Clone, Copy, Default)]
struct SnapUsage {
    five_hour: Option<RateLimitWindow>,
    seven_day: Option<RateLimitWindow>,
    cost_usd: Option<f64>,
    /// Claude context-window fill (0..=100), the tile header's meter (N1).
    context_used_pct: Option<f32>,
    ingested_at_ms: u64,
}

/// One meter row, precomputed as plain data for the render fn.
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    /// "Session" (5h) or "Weekly" (7d).
    pub label: &'static str,
    /// Used percentage (0..=100); `None` renders as "-".
    pub used_pct: Option<f32>,
    /// Remaining percentage, clamped 0..=100.
    pub left_pct: Option<f32>,
    /// Reset hint ("resets in 2h 15m" or the server's human text).
    pub resets: Option<String>,
    /// Fill color by remaining amount (red <= 10, amber <= 30, green above).
    pub color: (u8, u8, u8),
}

/// Everything the usage section renders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRows {
    pub claude: Vec<Meter>,
    pub codex: Vec<Meter>,
    /// Sum of per-session running costs from live statuslines, when any.
    pub total_cost_usd: Option<f64>,
}

/// Usage state: statusline snapshots folded per session + the two command reads.
#[derive(Debug, Default)]
pub struct UsageState {
    snapshots: HashMap<String, SnapUsage>,
    claude_cmd: Option<ClaudeUsage>,
    codex: Option<CodexUsage>,
}

impl UsageState {
    /// Fold one `status://snapshot` event (keeps the latest per session).
    pub fn fold_snapshot(&mut self, snap: &StatusSnapshot) {
        let e = self.snapshots.entry(snap.session_id.clone()).or_default();
        // Per-window: keep the last non-null reading rather than clobbering with
        // an absent block (partial snapshots are normal pre-first-response).
        if snap.five_hour.is_some() {
            e.five_hour = snap.five_hour;
        }
        if snap.seven_day.is_some() {
            e.seven_day = snap.seven_day;
        }
        if snap.cost_usd.is_some() {
            e.cost_usd = snap.cost_usd;
        }
        if snap.context_used_pct.is_some() {
            e.context_used_pct = snap.context_used_pct;
        }
        e.ingested_at_ms = snap.ingested_at_ms;
    }

    /// The latest context-window fill for a session UUID (the tile header's
    /// meter, N1). `None` until a snapshot with the field arrives.
    pub fn context_pct_of(&self, session_id: &str) -> Option<f32> {
        self.snapshots.get(session_id).and_then(|s| s.context_used_pct)
    }

    /// Fold a `claude_usage` command result (the cold-start fallback).
    pub fn fold_claude_cmd(&mut self, usage: ClaudeUsage) {
        self.claude_cmd = Some(usage);
    }

    /// Fold a `codex_usage` command result.
    pub fn fold_codex(&mut self, usage: CodexUsage) {
        self.codex = Some(usage);
    }

    /// True while no statusline rate-limit reading exists - the feed only runs
    /// the expensive `claude_usage` fallback in that state (webview parity).
    pub fn needs_claude_cmd_fallback(&self) -> bool {
        self.statusline_window(|s| s.five_hour).is_none()
            && self.statusline_window(|s| s.seven_day).is_none()
    }

    /// Freshest non-null statusline reading for one window, independently per
    /// window across all sessions (the webview's `selectStatuslineUsage`).
    fn statusline_window(
        &self,
        pick: impl Fn(&SnapUsage) -> Option<RateLimitWindow>,
    ) -> Option<WindowReading> {
        self.snapshots
            .values()
            .filter_map(|s| {
                let w = pick(s)?;
                Some(WindowReading {
                    used_pct: w.used_percentage?,
                    resets_at: w.resets_at,
                    ingested_at_ms: s.ingested_at_ms,
                })
            })
            .max_by_key(|r| r.ingested_at_ms)
    }

    /// Build the meter rows for rendering. `now_ms` drives Codex window rollover
    /// and reset-hint formatting.
    pub fn rows(&self, now_ms: u64) -> UsageRows {
        let now_s = (now_ms / 1000) as i64;

        // Claude: statusline first, command fallback second.
        let five = self.statusline_window(|s| s.five_hour);
        let seven = self.statusline_window(|s| s.seven_day);
        let claude = if five.is_some() || seven.is_some() {
            vec![
                meter("Weekly", seven.map(|w| w.used_pct), seven.and_then(|w| w.resets_at).map(|at| format!("resets {}", fmt_eta(now_s, at)))),
                meter("Session", five.map(|w| w.used_pct), five.and_then(|w| w.resets_at).map(|at| format!("resets {}", fmt_eta(now_s, at)))),
            ]
        } else {
            let c = self.claude_cmd.clone().unwrap_or_default();
            vec![
                meter("Weekly", c.week_used_pct, c.week_resets.map(|r| format!("resets {r}"))),
                meter("Session", c.session_used_pct, c.session_resets.map(|r| format!("resets {r}"))),
            ]
        };

        // Codex: only when a read succeeded; windows locally rolled forward.
        let codex = match &self.codex {
            Some(c) if c.ok => {
                let sec = c.secondary.map(|w| advance_window(w, now_s));
                let pri = c.primary.map(|w| advance_window(w, now_s));
                vec![
                    meter("Weekly", sec.and_then(|w| w.used_percent), sec.and_then(|w| w.resets_at).map(|at| format!("resets {}", fmt_eta(now_s, at)))),
                    meter("Session", pri.and_then(|w| w.used_percent), pri.and_then(|w| w.resets_at).map(|at| format!("resets {}", fmt_eta(now_s, at)))),
                ]
            }
            _ => Vec::new(),
        };

        let costs: Vec<f64> = self.snapshots.values().filter_map(|s| s.cost_usd).collect();
        let total_cost_usd = if costs.is_empty() { None } else { Some(costs.iter().sum()) };

        UsageRows { claude, codex, total_cost_usd }
    }
}

fn meter(label: &'static str, used_pct: Option<f32>, resets: Option<String>) -> Meter {
    let used = used_pct.map(|u| u.clamp(0.0, 100.0));
    let left = used.map(|u| 100.0 - u);
    Meter { label, used_pct: used, left_pct: left, resets, color: fill_color(left.unwrap_or(100.0)) }
}

/// Meter fill color by REMAINING percentage (webview `fillColor`).
pub fn fill_color(left_pct: f32) -> (u8, u8, u8) {
    if left_pct <= 10.0 {
        (248, 113, 113) // red
    } else if left_pct <= 30.0 {
        (251, 191, 36) // amber
    } else {
        (52, 211, 153) // green
    }
}

/// Roll a Codex window forward locally: once `resets_at` passes, the window is
/// fresh (`used = 0`) and resets at the next period boundary - so a stale read
/// shows "available" without waiting for the next poll (webview `advanceWindow`).
pub fn advance_window(w: CodexRateWindow, now_secs: i64) -> CodexRateWindow {
    let (Some(reset), Some(mins)) = (w.resets_at, w.window_minutes) else { return w };
    if now_secs <= reset || mins <= 0 {
        return w;
    }
    let period = mins * 60;
    let periods_passed = (now_secs - reset) / period + 1;
    CodexRateWindow {
        used_percent: Some(0.0),
        window_minutes: w.window_minutes,
        resets_at: Some(reset + periods_passed * period),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, five: Option<(f32, i64)>, seven: Option<(f32, i64)>, at: u64) -> StatusSnapshot {
        StatusSnapshot {
            session_id: id.to_string(),
            five_hour: five.map(|(u, r)| RateLimitWindow { resets_at: Some(r), used_percentage: Some(u) }),
            seven_day: seven.map(|(u, r)| RateLimitWindow { resets_at: Some(r), used_percentage: Some(u) }),
            rate_limits_present: five.is_some() || seven.is_some(),
            ingested_at_ms: at,
            ..Default::default()
        }
    }

    #[test]
    fn statusline_picks_freshest_non_null_per_window_independently() {
        let mut st = UsageState::default();
        // Session A: only a five-hour reading, fresh.
        st.fold_snapshot(&snap("a", Some((60.0, 1000)), None, 2000));
        // Session B: both windows, but older.
        st.fold_snapshot(&snap("b", Some((40.0, 1000)), Some((20.0, 5000)), 1000));
        let rows = st.rows(0);
        // Weekly comes from B (the only seven-day reading), Session from A (fresher).
        assert_eq!(rows.claude[0].label, "Weekly");
        assert_eq!(rows.claude[0].used_pct, Some(20.0));
        assert_eq!(rows.claude[1].label, "Session");
        assert_eq!(rows.claude[1].used_pct, Some(60.0));
    }

    #[test]
    fn a_partial_snapshot_does_not_clobber_an_earlier_window() {
        let mut st = UsageState::default();
        st.fold_snapshot(&snap("a", Some((60.0, 1000)), None, 1000));
        // Later snapshot from the same session without rate limits (routine).
        st.fold_snapshot(&snap("a", None, None, 2000));
        assert!(!st.needs_claude_cmd_fallback());
        assert_eq!(st.rows(0).claude[1].used_pct, Some(60.0));
    }

    #[test]
    fn command_fallback_is_used_only_without_statusline_data() {
        let mut st = UsageState::default();
        assert!(st.needs_claude_cmd_fallback());
        st.fold_claude_cmd(ClaudeUsage {
            session_used_pct: Some(10.0),
            week_used_pct: Some(30.0),
            week_resets: Some("Jun 20, 9pm".to_string()),
            ok: true,
            ..Default::default()
        });
        let rows = st.rows(0);
        assert_eq!(rows.claude[0].used_pct, Some(30.0));
        assert_eq!(rows.claude[0].resets.as_deref(), Some("resets Jun 20, 9pm"));

        // Statusline arrives: it takes over, fallback ignored.
        st.fold_snapshot(&snap("a", Some((75.0, 99)), None, 5000));
        assert!(!st.needs_claude_cmd_fallback());
        let rows = st.rows(0);
        assert_eq!(rows.claude[1].used_pct, Some(75.0));
    }

    #[test]
    fn advance_window_rolls_past_resets() {
        let w = CodexRateWindow { used_percent: Some(80.0), window_minutes: Some(300), resets_at: Some(1000) };
        // Before the reset: untouched.
        assert_eq!(advance_window(w, 999), w);
        // One period past: fresh, next boundary.
        let rolled = advance_window(w, 1001);
        assert_eq!(rolled.used_percent, Some(0.0));
        assert_eq!(rolled.resets_at, Some(1000 + 300 * 60));
        // Several periods past: lands on the boundary after `now`.
        let rolled = advance_window(w, 1000 + 3 * 300 * 60 + 5);
        assert_eq!(rolled.resets_at, Some(1000 + 4 * 300 * 60));
    }

    #[test]
    fn codex_rows_only_render_after_an_ok_read() {
        let mut st = UsageState::default();
        assert!(st.rows(0).codex.is_empty());
        st.fold_codex(CodexUsage { ok: false, ..Default::default() });
        assert!(st.rows(0).codex.is_empty());
        st.fold_codex(CodexUsage {
            primary: Some(CodexRateWindow { used_percent: Some(50.0), window_minutes: Some(300), resets_at: Some(10) }),
            ok: true,
            ..Default::default()
        });
        let rows = st.rows(0);
        assert_eq!(rows.codex.len(), 2);
        assert_eq!(rows.codex[1].used_pct, Some(50.0));
    }

    #[test]
    fn fill_color_thresholds() {
        assert_eq!(fill_color(5.0), (248, 113, 113));
        assert_eq!(fill_color(10.0), (248, 113, 113));
        assert_eq!(fill_color(30.0), (251, 191, 36));
        assert_eq!(fill_color(31.0), (52, 211, 153));
    }

    #[test]
    fn total_cost_sums_across_sessions() {
        let mut st = UsageState::default();
        let mut a = snap("a", None, None, 1);
        a.cost_usd = Some(1.25);
        let mut b = snap("b", None, None, 2);
        b.cost_usd = Some(0.75);
        st.fold_snapshot(&a);
        st.fold_snapshot(&b);
        assert_eq!(st.rows(0).total_cost_usd, Some(2.0));
    }
}
