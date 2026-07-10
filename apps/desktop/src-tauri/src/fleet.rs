//! Orchestrator wake - server-side fleet notifications.
//!
//! Problem this solves: the orchestrator (Cortana) had no push signal when a
//! supervised session - especially a captain - went idle, needed input, or
//! completed. It had to POLL the control socket, so captains got stranded
//! "waiting for the orchestrator" while the orchestrator did not know it was
//! being waited on. See `docs/ORCHESTRATOR-WAKE-DESIGN.md`.
//!
//! The only thing that re-invokes an idle Claude Code agent loop is typing a new
//! prompt into its PTY and submitting it (`tmux::send_text`). So the wake is a
//! server-side push: when a watched session transitions into an actionable state,
//! we inject a compact, routable line into the orchestrator's terminal.
//!
//! Two pieces:
//!   - [`FleetWatchRegistry`] - which orchestrators want wakes, and for what.
//!     All behaviour is OPT-IN: a fleet with no armed watch sees zero change, so
//!     this ships without a global default-off flag (the arming IS the opt-in).
//!   - [`FleetNotifier`] - the observer on the session-status edge stream that
//!     resolves the transition to a captain, coalesces, gates on the
//!     orchestrator being idle, and injects the wake.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::claude::StatusBridge;
use crate::control::CaptainsRegistry;
use crate::model::SessionStatus;

/// The default set of session states that warrant an orchestrator wake, mapping
/// the order's three buckets:
///   - idle / turn-complete -> `Completed`
///   - needs-input          -> `NeedsQuestion`, `NeedsPermission`
///   - completed / exited    -> `Completed`, `Failed`, `Expired`, plus `RateLimited`
///     (a blocked turn the orchestrator should attend to).
/// Deliberately excludes `Working` / `WaitingOnSubagents` (still busy) and
/// `Detached` / `Restoring` / `Unknown` (nothing actionable).
pub fn default_actionable_states() -> Vec<SessionStatus> {
    vec![
        SessionStatus::Completed,
        SessionStatus::NeedsQuestion,
        SessionStatus::NeedsPermission,
        SessionStatus::Failed,
        SessionStatus::RateLimited,
        SessionStatus::Expired,
    ]
}

/// Whether the orchestrator is at its prompt and safe to inject a wake into. Only
/// `Completed` (turn done, back at the main prompt) qualifies: injecting while the
/// orchestrator is `Working` / `WaitingOnSubagents` would pile onto an active turn,
/// and injecting at a `NeedsQuestion` / `NeedsPermission` prompt would answer the
/// wrong prompt. Everything else holds the wake until the next `Completed` edge.
fn is_ready_for_wake(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Completed)
}

/// Which sessions an orchestrator wants to be woken about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "sessions")]
pub enum WatchScope {
    /// Every claimed captain (the default, and the order's priority).
    Captains,
    /// Every supervised session/terminal, captain or not.
    All,
    /// An explicit list of tile ids.
    Sessions(Vec<String>),
}

impl WatchScope {
    /// Does this scope cover a session identified by its tile id + captain-ness?
    fn covers(&self, tile: &str, is_captain: bool) -> bool {
        match self {
            WatchScope::Captains => is_captain,
            WatchScope::All => true,
            WatchScope::Sessions(ids) => ids.iter().any(|s| s == tile),
        }
    }
}

/// One armed orchestrator watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetWatch {
    /// The orchestrator's tile id - where the wake is injected.
    pub orchestrator_tile_id: String,
    /// Which sessions to wake on.
    pub scope: WatchScope,
    /// Which states fire a wake (defaults to [`default_actionable_states`]).
    pub states: Vec<SessionStatus>,
}

impl FleetWatch {
    fn wants(&self, tile: &str, is_captain: bool, status: SessionStatus) -> bool {
        self.scope.covers(tile, is_captain) && self.states.contains(&status)
    }
}

/// The set of armed watches, keyed by orchestrator tile id. Thread-safe; shared
/// (`Arc`) between the control commands that arm/disarm it and the notifier that
/// reads it. In-memory only (unlike the persistent captains registry): a watch is
/// meaningful only while its orchestrator session is live, and a fresh orchestrator
/// re-arms on start.
#[derive(Default)]
pub struct FleetWatchRegistry {
    watches: Mutex<HashMap<String, FleetWatch>>,
}

impl FleetWatchRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm (or replace) a watch for `orchestrator_tile_id`. `states` empty falls
    /// back to the default actionable set. Returns the stored watch.
    pub fn arm(
        &self,
        orchestrator_tile_id: &str,
        scope: WatchScope,
        states: Vec<SessionStatus>,
    ) -> FleetWatch {
        let states = if states.is_empty() {
            default_actionable_states()
        } else {
            states
        };
        let watch = FleetWatch {
            orchestrator_tile_id: orchestrator_tile_id.to_string(),
            scope,
            states,
        };
        self.watches
            .lock()
            .expect("fleet watch mutex poisoned")
            .insert(orchestrator_tile_id.to_string(), watch.clone());
        watch
    }

    /// Disarm the watch for `orchestrator_tile_id`. Returns whether one existed.
    pub fn disarm(&self, orchestrator_tile_id: &str) -> bool {
        self.watches
            .lock()
            .expect("fleet watch mutex poisoned")
            .remove(orchestrator_tile_id)
            .is_some()
    }

    /// A snapshot of every armed watch (for `list_fleet_watches` + the notifier).
    pub fn snapshot(&self) -> Vec<FleetWatch> {
        let mut v: Vec<FleetWatch> = self
            .watches
            .lock()
            .expect("fleet watch mutex poisoned")
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.orchestrator_tile_id.cmp(&b.orchestrator_tile_id));
        v
    }

    pub fn is_empty(&self) -> bool {
        self.watches
            .lock()
            .expect("fleet watch mutex poisoned")
            .is_empty()
    }
}

/// A single pending wake, coalesced per (orchestrator, source session).
#[derive(Debug, Clone)]
struct WakeItem {
    session_tile: String,
    ship_slug: Option<String>,
    is_captain: bool,
    status: SessionStatus,
}

/// A closure that injects `text` into the terminal with tile id `tile`, submitting
/// it (Enter). Production wires this to `tmux::send_text`; tests record calls.
pub type Injector = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

/// An optional sink for the bonus `fleet://wake` UI/event payload.
pub type EventSink = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Default)]
struct NotifierState {
    /// Last status observed per tile - for edge detection (the observer fires on
    /// every emit, not only on edges) and for the orchestrator idle gate.
    last_status: HashMap<String, SessionStatus>,
    /// Coalesced pending wakes per orchestrator tile, keyed by source session tile
    /// so repeated transitions of the same session collapse to its latest state.
    pending: HashMap<String, HashMap<String, WakeItem>>,
    /// Orchestrators we have injected into and not yet re-observed at their prompt.
    /// Prevents a second inject within one idle window (before the journal reflects
    /// the orchestrator going Working). Cleared on the orchestrator's next
    /// `Completed` edge.
    suppressed: std::collections::HashSet<String>,
}

/// Turns supervised-session status edges into orchestrator wakes. Installed as the
/// `AgentBridge` status observer in `setup()`.
pub struct FleetNotifier {
    watches: Arc<FleetWatchRegistry>,
    captains: Arc<CaptainsRegistry>,
    status_bridge: Arc<StatusBridge>,
    inject: Injector,
    event_sink: Option<EventSink>,
    state: Mutex<NotifierState>,
}

impl FleetNotifier {
    pub fn new(
        watches: Arc<FleetWatchRegistry>,
        captains: Arc<CaptainsRegistry>,
        status_bridge: Arc<StatusBridge>,
        inject: Injector,
    ) -> Self {
        Self {
            watches,
            captains,
            status_bridge,
            inject,
            event_sink: None,
            state: Mutex::new(NotifierState::default()),
        }
    }

    pub fn with_event_sink(mut self, sink: EventSink) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Observe one session status emit `(session_uuid, status)`. This is the hot
    /// path wired to `AgentBridge::emit_session`. Fast-returns when no watch is
    /// armed (the common case) so an un-armed fleet pays almost nothing.
    pub fn on_status(&self, session_uuid: &str, status: SessionStatus) {
        if self.watches.is_empty() {
            return;
        }
        // Resolve the UUID (supervisor/emit key) to the tile id (tmux / captains /
        // watch key). Without a live tile binding we cannot route or gate.
        let Some(tile) = self.status_bridge.terminal_for_session(session_uuid) else {
            return;
        };

        let mut st = self.state.lock().expect("fleet notifier mutex poisoned");

        // Edge detection: the observer fires on every emit for a session, not just
        // on changes. Only act on an actual transition.
        if st.last_status.get(&tile) == Some(&status) {
            return;
        }
        st.last_status.insert(tile.clone(), status);

        let is_captain_record = self.captains.captain_for_session(&tile);
        let is_captain = is_captain_record.is_some();
        let ship_slug = is_captain_record.map(|c| c.ship_slug);

        let watches = self.watches.snapshot();
        for w in &watches {
            if w.orchestrator_tile_id == tile {
                // The orchestrator's OWN transition. Never a wake source (you don't
                // wake yourself); instead it drives the idle gate.
                if is_ready_for_wake(status) {
                    st.suppressed.remove(&tile);
                    self.try_flush(&mut st, &tile);
                }
                continue;
            }
            if w.wants(&tile, is_captain, status) {
                let entry = st
                    .pending
                    .entry(w.orchestrator_tile_id.clone())
                    .or_default();
                entry.insert(
                    tile.clone(),
                    WakeItem {
                        session_tile: tile.clone(),
                        ship_slug: ship_slug.clone(),
                        is_captain,
                        status,
                    },
                );
                let orch = w.orchestrator_tile_id.clone();
                self.try_flush(&mut st, &orch);
            }
        }
    }

    /// Inject the coalesced pending batch into `orch` if it is at its prompt and not
    /// already suppressed. Clears the batch and suppresses further injects until the
    /// orchestrator is next observed `Completed`.
    fn try_flush(&self, st: &mut NotifierState, orch: &str) {
        let ready = st
            .last_status
            .get(orch)
            .copied()
            .map(is_ready_for_wake)
            .unwrap_or(false);
        if !ready || st.suppressed.contains(orch) {
            return;
        }
        let Some(items_map) = st.pending.get(orch) else {
            return;
        };
        if items_map.is_empty() {
            return;
        }
        let mut items: Vec<WakeItem> = items_map.values().cloned().collect();
        items.sort_by(|a, b| a.session_tile.cmp(&b.session_tile));

        let text = wake_message(&items);
        // Emit the bonus UI/event payload regardless of injection outcome.
        if let Some(sink) = &self.event_sink {
            sink(&wake_event_payload(orch, &items));
        }
        match (self.inject)(orch, &text) {
            Ok(()) => {
                st.pending.remove(orch);
                st.suppressed.insert(orch.to_string());
            }
            Err(_e) => {
                // Injection failed (orchestrator terminal gone, tmux hiccup). Keep
                // the batch pending so a later idle edge retries; do not suppress.
            }
        }
    }
}

/// Render the wake prompt injected into the orchestrator's terminal. Compact and
/// machine-routable so the fleet-orchestrator skill can parse it, while reading as
/// a plain instruction.
fn wake_message(items: &[WakeItem]) -> String {
    let describe = |it: &WakeItem| -> String {
        let state = status_camel(it.status);
        if it.is_captain {
            let ship = it.ship_slug.as_deref().unwrap_or("?");
            format!("captain \"{ship}\" ({}) -> {state}", it.session_tile)
        } else {
            format!("session {} -> {state}", it.session_tile)
        }
    };
    if items.len() == 1 {
        let noun = if items[0].is_captain { "it" } else { "that session" };
        format!(
            "[T-HUB FLEET WAKE] {}. Supervise {noun} (get_status / read_terminal, then act).",
            describe(&items[0])
        )
    } else {
        let list = items.iter().map(describe).collect::<Vec<_>>().join("; ");
        format!(
            "[T-HUB FLEET WAKE] {} supervised sessions need you: {list}. Supervise them.",
            items.len()
        )
    }
}

/// The `fleet://wake` event payload (bonus UI badge / voice cue).
fn wake_event_payload(orchestrator_tile: &str, items: &[WakeItem]) -> Value {
    json!({
        "orchestrator": orchestrator_tile,
        "count": items.len(),
        "items": items.iter().map(|it| json!({
            "sessionId": it.session_tile,
            "shipSlug": it.ship_slug,
            "isCaptain": it.is_captain,
            "state": status_camel(it.status),
        })).collect::<Vec<_>>(),
    })
}

/// SessionStatus -> its camelCase wire string (matches `get_status` / IPC).
fn status_camel(status: SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A recording injector: captures (orch_tile, text) pairs so tests can assert
    /// exactly what was woken, and how many times.
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(String, String)>>,
        fail: AtomicUsize, // number of leading calls to fail
    }
    impl Recorder {
        fn injector(self: &Arc<Self>) -> Injector {
            let me = self.clone();
            Arc::new(move |orch: &str, text: &str| {
                if me.fail.load(Ordering::SeqCst) > 0 {
                    me.fail.fetch_sub(1, Ordering::SeqCst);
                    return Err("injected failure".into());
                }
                me.calls
                    .lock()
                    .unwrap()
                    .push((orch.to_string(), text.to_string()));
                Ok(())
            })
        }
        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// Wire a notifier with a captain + orchestrator already bound to tiles in the
    /// status bridge. Returns (notifier, recorder, watches, captains).
    fn harness() -> (
        FleetNotifier,
        Arc<Recorder>,
        Arc<FleetWatchRegistry>,
        Arc<CaptainsRegistry>,
    ) {
        let status = Arc::new(StatusBridge::new());
        // captain tile `capaaaaa` hosts uuid `u-cap`; orchestrator tile `orcbbbbb`
        // hosts uuid `u-orc`.
        status.ingest(
            "u-cap",
            &json!({ "cwd": "/p", "tmux_session": "th_capaaaaa" }),
            1,
        );
        status.ingest(
            "u-orc",
            &json!({ "cwd": "/o", "tmux_session": "th_orcbbbbb" }),
            2,
        );
        let captains = Arc::new(CaptainsRegistry::new());
        captains
            .claim("capaaaaa", Some("ship-alpha"), vec![])
            .unwrap();
        let watches = Arc::new(FleetWatchRegistry::new());
        let rec = Arc::new(Recorder::default());
        let notifier = FleetNotifier::new(
            watches.clone(),
            captains.clone(),
            status.clone(),
            rec.injector(),
        );
        (notifier, rec, watches, captains)
    }

    #[test]
    fn no_watch_armed_means_no_wake() {
        let (n, rec, _w, _c) = harness();
        n.on_status("u-cap", SessionStatus::Completed);
        assert!(rec.calls().is_empty(), "un-armed fleet must not wake");
    }

    #[test]
    fn captain_completion_wakes_an_idle_orchestrator() {
        let (n, rec, w, _c) = harness();
        w.arm("orcbbbbb", WatchScope::Captains, vec![]);
        // Orchestrator is at its prompt.
        n.on_status("u-orc", SessionStatus::Completed);
        // Captain finishes a turn.
        n.on_status("u-cap", SessionStatus::Completed);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1, "exactly one wake");
        assert_eq!(calls[0].0, "orcbbbbb", "injected into the orchestrator tile");
        assert!(
            calls[0].1.contains("capaaaaa") && calls[0].1.contains("completed"),
            "payload names the captain + state: {}",
            calls[0].1
        );
    }

    #[test]
    fn wake_holds_until_orchestrator_is_idle_then_coalesces() {
        let (n, rec, w, c) = harness();
        // A second captain, also watched.
        c.claim("capccccc", Some("ship-gamma"), vec![]).unwrap();
        n.status_bridge_ingest("u-cap2", "th_capccccc");
        w.arm("orcbbbbb", WatchScope::Captains, vec![]);

        // Orchestrator is BUSY (mid-turn). Two captains transition.
        n.on_status("u-orc", SessionStatus::Working);
        n.on_status("u-cap", SessionStatus::NeedsQuestion);
        n.on_status("u-cap2", SessionStatus::Completed);
        assert!(
            rec.calls().is_empty(),
            "no wake while the orchestrator is busy"
        );

        // Orchestrator returns to its prompt -> one coalesced wake for BOTH.
        n.on_status("u-orc", SessionStatus::Completed);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1, "coalesced into a single wake");
        assert!(calls[0].1.contains("capaaaaa"), "names captain 1");
        assert!(calls[0].1.contains("capccccc"), "names captain 2");
        assert!(calls[0].1.contains("2 supervised"), "coalesced header");
    }

    #[test]
    fn does_not_wake_on_the_orchestrators_own_transition() {
        let (n, rec, w, _c) = harness();
        // Scope All would otherwise match the orchestrator itself.
        w.arm("orcbbbbb", WatchScope::All, vec![]);
        n.on_status("u-orc", SessionStatus::Completed);
        assert!(
            rec.calls().is_empty(),
            "an orchestrator must never wake itself"
        );
    }

    #[test]
    fn only_one_wake_per_idle_window() {
        let (n, rec, w, _c) = harness();
        w.arm("orcbbbbb", WatchScope::Captains, vec![]);
        n.on_status("u-orc", SessionStatus::Completed);
        // First captain transition -> wake, orchestrator now suppressed (busy).
        n.on_status("u-cap", SessionStatus::NeedsQuestion);
        // Another transition of the same captain BEFORE the orchestrator idles again
        // must NOT inject a second time.
        n.on_status("u-cap", SessionStatus::Completed);
        assert_eq!(rec.calls().len(), 1, "no double-inject within one idle window");
        // Orchestrator goes busy then idle -> the held transition flushes.
        n.on_status("u-orc", SessionStatus::Working);
        n.on_status("u-orc", SessionStatus::Completed);
        assert_eq!(rec.calls().len(), 2, "the next idle edge flushes the held state");
    }

    #[test]
    fn scope_all_wakes_on_a_non_captain_session() {
        let (n, rec, w, _c) = harness();
        w.arm("orcbbbbb", WatchScope::All, vec![]);
        n.on_status("u-orc", SessionStatus::Completed);
        // A plain (non-captain) session: bind a tile, then transition it.
        n.status_bridge_ingest("u-plain", "th_plainxxx");
        n.on_status("u-plain", SessionStatus::NeedsPermission);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.contains("session plainxxx"),
            "non-captain phrasing: {}",
            calls[0].1
        );
    }

    #[test]
    fn failed_injection_is_retried_on_the_next_idle_edge() {
        let (n, rec, w, _c) = harness();
        rec.fail.store(1, Ordering::SeqCst); // fail the first inject only
        w.arm("orcbbbbb", WatchScope::Captains, vec![]);
        n.on_status("u-orc", SessionStatus::Completed);
        n.on_status("u-cap", SessionStatus::Completed);
        assert!(rec.calls().is_empty(), "first inject failed, batch retained");
        // A fresh idle edge retries; this time it succeeds.
        n.on_status("u-orc", SessionStatus::Working);
        n.on_status("u-orc", SessionStatus::Completed);
        assert_eq!(rec.calls().len(), 1, "retry delivered the held wake");
    }

    // A tiny test helper on the notifier so tests can bind extra tiles into the
    // shared status bridge after construction.
    impl FleetNotifier {
        fn status_bridge_ingest(&self, uuid: &str, tmux_session: &str) {
            self.status_bridge
                .ingest(uuid, &json!({ "cwd": "/x", "tmux_session": tmux_session }), 9);
        }
    }

    /// END-TO-END: a captain going idle WAKES the orchestrator, proven against
    /// REAL tmux. The notifier's real injector types the wake line into a live
    /// orchestrator pane; we read it back with capture-pane. `send_text` + Enter
    /// is exactly what re-invokes an idle Claude Code loop, so a wake landing in
    /// the pane is the loop being woken.
    ///
    /// Gated on an ISOLATED tmux socket so a plain `cargo test` never creates
    /// sessions on a live T-Hub app's `t-hub` socket. Run it with:
    ///   T_HUB_TMUX_SOCKET=t-hub-e2e-wake cargo test --lib wake_lands_in_a_real
    #[test]
    fn wake_lands_in_a_real_orchestrator_pane_e2e() {
        use crate::tmux;
        // Opt-in: this E2E is only meaningful when the operator EXPLICITLY points
        // it at an isolated socket. Gate on the env override being set, not on the
        // resolved socket value - a `cargo test` build now defaults `socket()` to
        // the isolated `t-hub-test` (see `tmux::SOCKET_NAME`), so a value check
        // would auto-run this heavy E2E in the normal suite. Requiring the explicit
        // env keeps it opt-in AND still guarantees it never touches the live socket.
        if std::env::var("T_HUB_TMUX_SOCKET").is_err() {
            eprintln!(
                "fleet e2e: set T_HUB_TMUX_SOCKET to an isolated name to run this \
                 E2E (e.g. t-hub-e2e-wake) - skipping"
            );
            return;
        }
        let cap_tile = "e2ecap01";
        let orc_tile = "e2eorc01";
        let cap_sess = tmux::target_for_id(cap_tile); // th_e2ecap01
        let orc_sess = tmux::target_for_id(orc_tile);
        let _ = tmux::kill_session(&cap_sess);
        let _ = tmux::kill_session(&orc_sess);
        // `cat` keeps the pane alive and echoes typed lines with no shell prompt
        // offset / command execution to mangle the captured text.
        if tmux::new_session(&orc_sess, "/tmp", Some("cat")).is_err() {
            eprintln!("fleet e2e: tmux new-session failed (tmux missing?) - skipping");
            return;
        }
        tmux::new_session(&cap_sess, "/tmp", Some("cat")).expect("captain session");

        let status = Arc::new(StatusBridge::new());
        status.ingest(
            "u-e2e-cap",
            &json!({ "cwd": "/tmp", "tmux_session": cap_sess }),
            1,
        );
        status.ingest(
            "u-e2e-orc",
            &json!({ "cwd": "/tmp", "tmux_session": orc_sess }),
            2,
        );
        let captains = Arc::new(CaptainsRegistry::new());
        captains.claim(cap_tile, Some("ship-e2e"), vec![]).unwrap();
        let watches = Arc::new(FleetWatchRegistry::new());
        watches.arm(orc_tile, WatchScope::Captains, vec![]);

        // The REAL injector: type + submit into the tile's tmux pane.
        let inject: Injector = Arc::new(|tile: &str, text: &str| {
            tmux::send_text(&tmux::target_for_id(tile), text, true).map_err(|e| e.to_string())
        });
        let notifier = FleetNotifier::new(watches, captains, status, inject);

        // Orchestrator at its prompt (idle); captain finishes its turn.
        notifier.on_status("u-e2e-orc", SessionStatus::Completed);
        notifier.on_status("u-e2e-cap", SessionStatus::Completed);

        std::thread::sleep(std::time::Duration::from_millis(400));
        let pane = tmux::capture_pane_text(&orc_sess, 30).expect("capture orchestrator pane");

        let _ = tmux::kill_session(&cap_sess);
        let _ = tmux::kill_session(&orc_sess);

        eprintln!("--- orchestrator pane after a captain went idle ---\n{pane}\n--- end pane ---");
        assert!(
            pane.contains("T-HUB FLEET WAKE") && pane.contains("e2ecap01"),
            "the orchestrator pane must show the injected wake naming the captain; got:\n{pane}"
        );
    }
}
