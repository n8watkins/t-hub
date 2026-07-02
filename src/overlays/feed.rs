//! The overlay data pump (T9): background threads that keep [`SidebarState`]
//! fresh off the §1.3 ControlClient.
//!
//! Two threads per feed:
//!  - **events**: drains `ControlClient::events()` through [`super::fold_event`].
//!    NOTE the wire's event channel is competing-consumer (one message, one
//!    receiver) - the feed must be the process's only drainer.
//!  - **poll**: runs the command cadences via [`PollPlan`] (pure, unit-tested)
//!    and services UI actions ([`OverlayAction`]) between ticks.
//!
//! Actions the feed cannot fulfill itself - resuming a session means spawning a
//! terminal tile, which the workspace shell (T8) owns - surface on the
//! [`HostRequest`] channel for the host to wire.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use serde_json::json;

use super::metrics::{HostMetrics, METRICS_POLL_MS};
use super::model::now_ms;
use super::recents::RecentEntry;
use super::supervision::SupervisionTree;
use super::usage::{ClaudeUsage, CodexUsage};
use super::{fold_event, SidebarState};
use crate::apply::{self, ApplyCommand};
use crate::wire::ControlClient;

/// Recents refresh cadence (the webview refreshes on mount + window focus; a
/// native poll approximates the focus trigger).
pub const RECENTS_POLL_MS: u64 = 60_000;
/// Codex usage cadence (webview `POLL_MS`).
pub const CODEX_POLL_MS: u64 = 5 * 60_000;
/// Min gap between `claude_usage` attempts (webview `USAGE_RETRY_GAP_MS`).
pub const CLAUDE_ATTEMPT_GAP_MS: u64 = 60_000;
/// A successful `claude_usage` read stays fresh this long (webview `CLAUDE_FRESH_MS`).
pub const CLAUDE_FRESH_MS: u64 = 60 * 60_000;
/// `list_terminals` cadence (drives the recents open-project filter).
pub const LIVE_SESSIONS_POLL_MS: u64 = 10_000;
/// Poll-thread tick (also the action-servicing latency bound).
const TICK_MS: u64 = 500;

/// UI-initiated actions, sent by the render layer.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayAction {
    /// Resume a recent session (`claude --resume <id>` in `cwd`). Gated by the
    /// 1.5s double-spawn window, then forwarded as a [`HostRequest`].
    Resume { session_id: String, cwd: String },
    /// Dismiss a recent project: optimistic local hide + the durable
    /// `archive_recent_project` command + a refresh.
    Archive { cwd: String },
    /// Force a recents refetch now.
    RefreshRecents,
}

/// Work only the embedding shell can do. T8 wires this receiver to its
/// workspace model (see the mount contract in §5 of the execution doc).
#[derive(Debug, Clone, PartialEq)]
pub enum HostRequest {
    /// Spawn a terminal tile at `cwd` running `claude --resume <session_id>`.
    /// (The socket's `spawn_terminal` only forwards to a connected UI sink and
    /// carries no command argument, so the server cannot fulfill this.)
    ResumeSession { session_id: String, cwd: String },
}

/// Which command fetch is due (see [`PollPlan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    Metrics,
    Recents,
    LiveSessions,
    Codex,
    Claude,
    SupervisionSeed,
}

/// Pure cadence scheduler for the poll thread. Deterministic (caller passes
/// `now_ms`), so every gating rule unit-tests without threads or sockets.
#[derive(Debug, Default)]
pub struct PollPlan {
    next_metrics_ms: u64,
    next_recents_ms: u64,
    next_live_ms: u64,
    next_codex_ms: u64,
    next_claude_attempt_ms: u64,
    claude_last_good_ms: Option<u64>,
    supervision_seeded: bool,
}

impl PollPlan {
    /// Everything is due immediately on a fresh plan.
    pub fn new() -> Self {
        Self::default()
    }

    /// The fetches due at `now_ms`, arming each one's next deadline.
    /// `needs_claude_fallback` gates the expensive `claude_usage` command: it
    /// only ever runs while no statusline reading exists, at most once per
    /// [`CLAUDE_ATTEMPT_GAP_MS`], and not while a prior success is fresh.
    pub fn due(&mut self, now_ms: u64, needs_claude_fallback: bool) -> Vec<Fetch> {
        let mut out = Vec::new();
        if !self.supervision_seeded {
            self.supervision_seeded = true;
            out.push(Fetch::SupervisionSeed);
        }
        if now_ms >= self.next_metrics_ms {
            self.next_metrics_ms = now_ms + METRICS_POLL_MS;
            out.push(Fetch::Metrics);
        }
        if now_ms >= self.next_recents_ms {
            self.next_recents_ms = now_ms + RECENTS_POLL_MS;
            out.push(Fetch::Recents);
        }
        if now_ms >= self.next_live_ms {
            self.next_live_ms = now_ms + LIVE_SESSIONS_POLL_MS;
            out.push(Fetch::LiveSessions);
        }
        if now_ms >= self.next_codex_ms {
            self.next_codex_ms = now_ms + CODEX_POLL_MS;
            out.push(Fetch::Codex);
        }
        let claude_fresh = self
            .claude_last_good_ms
            .is_some_and(|good| now_ms.saturating_sub(good) < CLAUDE_FRESH_MS);
        if needs_claude_fallback && now_ms >= self.next_claude_attempt_ms && !claude_fresh {
            self.next_claude_attempt_ms = now_ms + CLAUDE_ATTEMPT_GAP_MS;
            out.push(Fetch::Claude);
        }
        out
    }

    /// Note a successful `claude_usage` parse (arms the freshness gate).
    pub fn note_claude_ok(&mut self, now_ms: u64) {
        self.claude_last_good_ms = Some(now_ms);
    }

    /// Make the next tick refetch recents (after an archive, or on demand).
    pub fn force_recents(&mut self) {
        self.next_recents_ms = 0;
    }
}

/// The running feed: shared state + the action/host channels. Cheap to clone;
/// all clones share the same underlying feed.
#[derive(Clone)]
pub struct OverlayFeed {
    state: Arc<Mutex<SidebarState>>,
    actions_tx: Sender<OverlayAction>,
    host_rx: Receiver<HostRequest>,
    apply_rx: Receiver<ApplyCommand>,
}

impl OverlayFeed {
    /// Start the feed on `client`: arms the toast warmup, spawns the event and
    /// poll threads, and returns the shared handle. Threads run for the process
    /// lifetime (they own a clone of the client Arc).
    pub fn spawn(client: Arc<ControlClient>) -> Self {
        let state = Arc::new(Mutex::new(SidebarState::default()));
        state.lock().toasts.arm_warmup(now_ms());

        let (actions_tx, actions_rx) = unbounded::<OverlayAction>();
        let (host_tx, host_rx) = unbounded::<HostRequest>();
        let (apply_tx, apply_rx) = unbounded::<ApplyCommand>();

        // Event thread: the process's single event drainer. `control://apply`
        // frames (T12: the server's Organization-forward broadcast) are decoded
        // onto the apply channel for the cockpit worker; folding them too keeps
        // `events_folded` bumping, which is exactly the hint that short-circuits
        // the worker's next reconcile.
        {
            let state = state.clone();
            let rx = client.events();
            thread::spawn(move || {
                while let Ok(ev) = rx.recv() {
                    if ev.channel == apply::APPLY_CHANNEL {
                        match apply::parse_event(&ev.payload) {
                            Some(cmd) => {
                                let _ = apply_tx.send(cmd);
                            }
                            None => log::debug!(
                                "control://apply frame with no native arm: {}",
                                ev.payload
                            ),
                        }
                    }
                    fold_event(&mut state.lock(), &ev.channel, &ev.payload, now_ms());
                }
            });
        }

        // Poll thread: cadences + action servicing.
        {
            let state = state.clone();
            let client = client.clone();
            let host_tx = host_tx.clone();
            thread::spawn(move || {
                let mut plan = PollPlan::new();
                loop {
                    let needs_fallback = state.lock().usage.needs_claude_cmd_fallback();
                    for fetch in plan.due(now_ms(), needs_fallback) {
                        run_fetch(fetch, &client, &state, &mut plan);
                    }
                    match actions_rx.recv_timeout(Duration::from_millis(TICK_MS)) {
                        Ok(action) => run_action(action, &client, &state, &mut plan, &host_tx),
                        Err(crossbeam::channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam::channel::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
        }

        OverlayFeed { state, actions_tx, host_rx, apply_rx }
    }

    /// The shared sidebar state (lock, read/reduce, drop the guard before paint).
    pub fn state(&self) -> Arc<Mutex<SidebarState>> {
        self.state.clone()
    }

    /// Send a UI action (fire-and-forget).
    pub fn send(&self, action: OverlayAction) {
        let _ = self.actions_tx.send(action);
    }

    /// Requests only the embedding shell can fulfill (T8 wires this).
    pub fn host_requests(&self) -> Receiver<HostRequest> {
        self.host_rx.clone()
    }

    /// Organization applies decoded off the event stream (T12): the cockpit
    /// worker drains these into the chrome model. Same competing-consumer
    /// caveat as the wire's event channel - exactly one drainer.
    pub fn apply_requests(&self) -> Receiver<ApplyCommand> {
        self.apply_rx.clone()
    }

    /// Tab-aware toast suppression: the sessions the user is currently looking
    /// at (either id space - Claude session UUIDs and/or `th_*` tmux names).
    /// T8 calls this on tab/focus changes.
    pub fn set_active_sessions(&self, sessions: HashSet<String>) {
        self.state.lock().toasts.set_active_sessions(sessions);
    }
}

fn run_fetch(
    fetch: Fetch,
    client: &ControlClient,
    state: &Arc<Mutex<SidebarState>>,
    plan: &mut PollPlan,
) {
    match fetch {
        Fetch::Metrics => match client.request("host_metrics", json!({})) {
            Ok(v) => match serde_json::from_value::<HostMetrics>(v) {
                Ok(m) => state.lock().metrics.fold_metrics(m, now_ms()),
                Err(e) => state.lock().metrics.fold_error(format!("bad host_metrics shape: {e}")),
            },
            // Expected until the agent bridge connects; shown as a gentle hint.
            Err(e) => state.lock().metrics.fold_error(e.to_string()),
        },
        Fetch::Recents => match client.request("recent_sessions", json!({})) {
            Ok(v) => match serde_json::from_value::<Vec<RecentEntry>>(v) {
                Ok(list) => state.lock().recents.fold_list(list),
                Err(e) => state.lock().recents.fold_error(format!("bad recent_sessions shape: {e}")),
            },
            Err(e) => state.lock().recents.fold_error(e.to_string()),
        },
        Fetch::LiveSessions => {
            if let Ok(v) = client.request("list_terminals", json!({})) {
                let live: HashSet<String> = v["terminals"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|t| t["tmuxSession"].as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                state.lock().index.set_live_tmux(live);
            }
        }
        Fetch::Codex => {
            if let Ok(v) = client.request("codex_usage", json!({})) {
                if let Ok(u) = serde_json::from_value::<CodexUsage>(v) {
                    state.lock().usage.fold_codex(u);
                }
            }
        }
        Fetch::Claude => {
            if let Ok(v) = client.request("claude_usage", json!({})) {
                if let Ok(u) = serde_json::from_value::<ClaudeUsage>(v) {
                    if u.ok {
                        plan.note_claude_ok(now_ms());
                    }
                    state.lock().usage.fold_claude_cmd(u);
                }
            }
        }
        Fetch::SupervisionSeed => {
            let ids: Vec<String> = client
                .request("supervision_session_ids", json!({}))
                .ok()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            for id in ids {
                let Ok(v) = client.request("supervision_tree", json!({ "sessionId": id })) else {
                    continue;
                };
                if v.is_null() {
                    continue;
                }
                if let Ok(tree) = serde_json::from_value::<SupervisionTree>(v) {
                    let mut st = state.lock();
                    // Seeded statuses are point-in-time state, not transitions:
                    // baseline the toast dedup without ever toasting.
                    st.toasts.seed_status(&tree.session_id, tree.status);
                    st.supervision.fold_tree(tree, now_ms());
                }
            }
        }
    }
}

fn run_action(
    action: OverlayAction,
    client: &ControlClient,
    state: &Arc<Mutex<SidebarState>>,
    plan: &mut PollPlan,
    host_tx: &Sender<HostRequest>,
) {
    match action {
        OverlayAction::Resume { session_id, cwd } => {
            if state.lock().recents.begin_resume(now_ms()) {
                let _ = host_tx.send(HostRequest::ResumeSession { session_id, cwd });
            }
        }
        OverlayAction::Archive { cwd } => {
            state.lock().recents.hide(&cwd);
            if client.request("archive_recent_project", json!({ "cwd": cwd })).is_ok() {
                plan.force_recents();
            }
        }
        OverlayAction::RefreshRecents => plan.force_recents(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_plan_fetches_everything_once_then_respects_cadence() {
        let mut plan = PollPlan::new();
        let first = plan.due(0, false);
        assert_eq!(
            first,
            vec![
                Fetch::SupervisionSeed,
                Fetch::Metrics,
                Fetch::Recents,
                Fetch::LiveSessions,
                Fetch::Codex
            ]
        );
        // Immediately after: nothing due (and the seed never repeats).
        assert!(plan.due(1, false).is_empty());
        // Metrics comes due first.
        assert_eq!(plan.due(METRICS_POLL_MS, false), vec![Fetch::Metrics]);
        // Live sessions at 10s (plus the next metrics tick).
        let at_10s = plan.due(LIVE_SESSIONS_POLL_MS, false);
        assert!(at_10s.contains(&Fetch::LiveSessions));
        assert!(!at_10s.contains(&Fetch::Recents));
    }

    #[test]
    fn claude_fallback_gating() {
        let mut plan = PollPlan::new();
        plan.due(0, false); // burn the initial round (no fallback needed yet)

        // Needed: attempt fires, then the attempt gap gates.
        assert!(plan.due(1000, true).contains(&Fetch::Claude));
        assert!(!plan.due(1001, true).contains(&Fetch::Claude));
        assert!(plan
            .due(1000 + CLAUDE_ATTEMPT_GAP_MS, true)
            .contains(&Fetch::Claude));

        // A success stays fresh for an hour, even if still "needed".
        let t = 1000 + CLAUDE_ATTEMPT_GAP_MS;
        plan.note_claude_ok(t);
        assert!(!plan.due(t + CLAUDE_ATTEMPT_GAP_MS, true).contains(&Fetch::Claude));
        assert!(plan.due(t + CLAUDE_FRESH_MS, true).contains(&Fetch::Claude));

        // Not needed (statusline took over): never attempted.
        assert!(!plan.due(t + 2 * CLAUDE_FRESH_MS, false).contains(&Fetch::Claude));
    }

    #[test]
    fn force_recents_pulls_the_next_fetch_forward() {
        let mut plan = PollPlan::new();
        plan.due(0, false);
        assert!(!plan.due(1000, false).contains(&Fetch::Recents));
        plan.force_recents();
        assert!(plan.due(1001, false).contains(&Fetch::Recents));
        // ...and re-arms the normal cadence after.
        assert!(!plan.due(1002, false).contains(&Fetch::Recents));
    }
}
