//! Sidebar overlays (T9): recents, usage, host metrics, supervision, toasts.
//!
//! Layering (the §3 brief's "state logic gpui-free" rule):
//!  - one module per overlay ([`recents`], [`usage`], [`metrics`], [`supervision`],
//!    [`toasts`]): payload parsing + a state struct + reducers + a plain-data
//!    view-model, all unit-testable under `--no-default-features`;
//!  - [`SidebarState`] composes the five plus the cross-overlay session index,
//!    and [`fold_event`] is the single event reducer feeding them;
//!  - [`feed::OverlayFeed`] owns the ControlClient I/O (command polls + the event
//!    subscription) on background threads, writing into a shared `SidebarState`;
//!  - [`view::OverlaySidebar`] (feature `gui`) is the exported element that T8's
//!    sidebar shell mounts below the workspace list. gpui appears ONLY there.
//!
//! Data sources (all §1.2/§1.3, M3 already server-side): commands
//! `recent_sessions`, `claude_usage`, `codex_usage`, `host_metrics`,
//! `supervision_session_ids`, `supervision_tree`, `list_terminals`,
//! `archive_recent_project`; channels `status://snapshot`, `session://status`,
//! `supervision://tree`, `agent://title`, `agent://state`.

pub mod alerts;
pub mod feed;
pub mod metrics;
pub mod model;
pub mod recents;
pub mod supervision;
pub mod toasts;
pub mod usage;

#[cfg(feature = "gui")]
pub mod view;

#[cfg(feature = "gui")]
pub use view::OverlaySidebar;

pub use feed::{HostRequest, OverlayAction, OverlayFeed};

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use model::{SessionStatusEvent, SessionTitleEvent, StatusSnapshot};
use supervision::SupervisionTree;

/// Cross-overlay session identity (§1.2's two id spaces): Claude session UUIDs
/// (status/supervision keys) vs tmux-derived tile names (`th_*`). Snapshots carry
/// both, so this index is built from them, plus the live-session set from
/// `list_terminals`.
#[derive(Debug, Default)]
pub struct SessionIndex {
    tmux_by_id: HashMap<String, String>,
    cwd_by_id: HashMap<String, String>,
    live_tmux: HashSet<String>,
}

impl SessionIndex {
    pub fn fold_snapshot(&mut self, snap: &StatusSnapshot) {
        if let Some(tmux) = &snap.tmux_session {
            self.tmux_by_id.insert(snap.session_id.clone(), tmux.clone());
        }
        if let Some(cwd) = &snap.cwd {
            self.cwd_by_id
                .insert(snap.session_id.clone(), recents::norm_cwd(cwd).to_string());
        }
    }

    /// Replace the live tmux-session set (from a `list_terminals` poll).
    pub fn set_live_tmux(&mut self, live: HashSet<String>) {
        self.live_tmux = live;
    }

    /// The other-id-space name for a session UUID (its `th_*` tmux session).
    pub fn alias_of(&self, session_id: &str) -> Option<&str> {
        self.tmux_by_id.get(session_id).map(|s| s.as_str())
    }

    /// Every known (session UUID, `th_*` tmux session) pair. The T8 chrome uses
    /// this to resolve per-tile semantic status for its header dots.
    pub fn tmux_aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tmux_by_id.iter().map(|(id, tmux)| (id.as_str(), tmux.as_str()))
    }

    /// Normalized cwds of sessions whose tmux session is currently alive - the
    /// recents open-project filter. (The webview filters on open TILES; without
    /// a workspace model, live tmux is the closest server-derived signal - and
    /// it also hides projects whose session is detached-but-running, where a
    /// `--resume` would fork the live conversation. Documented §5.)
    pub fn open_cwds(&self) -> HashSet<String> {
        self.cwd_by_id
            .iter()
            .filter(|(id, _)| {
                self.tmux_by_id.get(*id).is_some_and(|t| self.live_tmux.contains(t))
            })
            .map(|(_, cwd)| cwd.clone())
            .collect()
    }
}

/// Everything the sidebar renders: the five overlay states + shared indexes.
/// Owned behind `Arc<Mutex<..>>` by [`feed::OverlayFeed`]; written by its
/// background threads, read by the render pass each frame.
#[derive(Debug, Default)]
pub struct SidebarState {
    pub recents: recents::RecentsState,
    pub usage: usage::UsageState,
    pub metrics: metrics::MetricsState,
    pub supervision: supervision::SupervisionState,
    pub toasts: toasts::ToastsState,
    pub index: SessionIndex,
    /// Latest `agent://state` connection string ("live", "replaying", ...).
    pub agent_connection: Option<String>,
    /// Total events folded (probe/debug visibility, not rendered).
    pub events_folded: u64,
}

/// Fold one control-socket event into the sidebar state. Malformed payloads are
/// ignored (a single bad frame must never tear the sidebar), unknown channels
/// fall through untouched.
pub fn fold_event(state: &mut SidebarState, channel: &str, payload: &Value, now_ms: u64) {
    state.events_folded += 1;
    match channel {
        "status://snapshot" => {
            let Ok(snap) = serde_json::from_value::<StatusSnapshot>(payload.clone()) else {
                return;
            };
            state.usage.fold_snapshot(&snap);
            state.index.fold_snapshot(&snap);
        }
        "session://status" => {
            let Ok(ev) = serde_json::from_value::<SessionStatusEvent>(payload.clone()) else {
                return;
            };
            state.supervision.fold_status(&ev.session_id, ev.status, now_ms);
            let alias = state.index.alias_of(&ev.session_id).map(|s| s.to_string());
            state.toasts.fold_status(&ev.session_id, ev.status, alias.as_deref(), now_ms);
        }
        "supervision://tree" => {
            let Ok(tree) = serde_json::from_value::<SupervisionTree>(payload.clone()) else {
                return;
            };
            // The tree carries the same status transition `session://status`
            // does; the toast dedup makes folding both a no-op when duplicated
            // (webview parity: notify.ts listens to both).
            let alias = state.index.alias_of(&tree.session_id).map(|s| s.to_string());
            state.toasts.fold_status(&tree.session_id, tree.status, alias.as_deref(), now_ms);
            state.supervision.fold_tree(tree, now_ms);
        }
        "agent://title" => {
            let Ok(ev) = serde_json::from_value::<SessionTitleEvent>(payload.clone()) else {
                return;
            };
            state.supervision.fold_title(&ev.session_id, &ev.title);
        }
        "agent://state" => {
            let Some(conn) = payload.get("connection").and_then(|v| v.as_str()) else {
                return;
            };
            // A replay re-emits historical statuses; re-arm the warmup window so
            // they seed baselines instead of toasting (deviation from the webview,
            // whose warmup only covers first connect - documented §5).
            if matches!(conn, "handshaking" | "replaying")
                && state.agent_connection.as_deref() != Some(conn)
            {
                state.toasts.arm_warmup(now_ms);
            }
            state.agent_connection = Some(conn.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::SessionStatus;
    use serde_json::json;
    use toasts::WARMUP_INITIAL_MS;

    #[test]
    fn snapshot_events_feed_usage_and_the_index() {
        let mut st = SidebarState::default();
        fold_event(
            &mut st,
            "status://snapshot",
            &json!({
                "sessionId": "uuid-1",
                "tmuxSession": "th_deadbeef",
                "cwd": "/p/app/",
                "fiveHour": {"resetsAt": 100, "usedPercentage": 40.0},
                "rateLimitsPresent": true,
                "ingestedAtMs": 5000
            }),
            1000,
        );
        assert_eq!(st.index.alias_of("uuid-1"), Some("th_deadbeef"));
        assert!(!st.usage.needs_claude_cmd_fallback());
        // The project only counts as open once its tmux session is live.
        assert!(st.index.open_cwds().is_empty());
        st.index.set_live_tmux(["th_deadbeef".to_string()].into());
        assert!(st.index.open_cwds().contains("/p/app"));
    }

    #[test]
    fn a_status_transition_after_warmup_updates_supervision_and_toasts() {
        let mut st = SidebarState::default();
        let now = WARMUP_INITIAL_MS + 1000;
        fold_event(
            &mut st,
            "session://status",
            &json!({"sessionId": "uuid-1", "status": "needsQuestion"}),
            now,
        );
        assert_eq!(st.supervision.status_of("uuid-1"), SessionStatus::NeedsQuestion);
        assert_eq!(st.toasts.visible().len(), 1);
    }

    #[test]
    fn tree_events_upsert_the_tree_and_dedup_against_status_events() {
        let mut st = SidebarState::default();
        let now = WARMUP_INITIAL_MS + 1000;
        fold_event(
            &mut st,
            "session://status",
            &json!({"sessionId": "uuid-1", "status": "completed"}),
            now,
        );
        // The tree snapshot carrying the SAME status must not double-toast.
        fold_event(
            &mut st,
            "supervision://tree",
            &json!({
                "sessionId": "uuid-1",
                "status": "completed",
                "children": [{
                    "parentSessionId": "uuid-1",
                    "agentId": "a1",
                    "state": "completed",
                    "startedAt": 0,
                    "endedAt": 500
                }],
                "outstandingTasks": 0
            }),
            now + 10,
        );
        assert_eq!(st.toasts.visible().len(), 1);
        assert_eq!(st.supervision.active().len(), 1);
    }

    #[test]
    fn title_events_relabel_and_agent_replay_rearms_warmup() {
        let mut st = SidebarState::default();
        let now = WARMUP_INITIAL_MS + 1000;
        fold_event(&mut st, "agent://title", &json!({"sessionId": "u", "title": "Fix the bug"}), now);
        fold_event(
            &mut st,
            "supervision://tree",
            &json!({"sessionId": "u", "status": "working", "children": [], "outstandingTasks": 1}),
            now,
        );
        assert_eq!(st.supervision.active()[0].label, "Fix the bug");

        assert!(!st.toasts.in_warmup(now));
        fold_event(&mut st, "agent://state", &json!({"connection": "replaying", "journalCursor": 5}), now);
        assert!(st.toasts.in_warmup(now + 1));
        assert_eq!(st.agent_connection.as_deref(), Some("replaying"));
        // A repeated "replaying" frame must not keep extending the window.
        fold_event(&mut st, "agent://state", &json!({"connection": "replaying"}), now + 100);
        assert!(!st.toasts.in_warmup(now + WARMUP_INITIAL_MS + 1));
    }

    #[test]
    fn malformed_payloads_and_unknown_channels_are_ignored() {
        let mut st = SidebarState::default();
        for (ch, payload) in [
            ("status://snapshot", json!({"nope": true})), // missing sessionId
            ("session://status", json!("just a string")),
            ("supervision://tree", json!(42)),
            ("agent://title", json!({})),
            ("agent://state", json!({})),
            ("something://new", json!({"whatever": 1})),
        ] {
            fold_event(&mut st, ch, &payload, 1000);
        }
        assert_eq!(st.events_folded, 6);
        assert!(st.supervision.active().is_empty());
        assert!(st.toasts.visible().is_empty());
    }
}
