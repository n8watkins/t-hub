//! Parallel-agent supervision (PLAN.md Workstream C — **the user's #1
//! priority**): model the orchestrator→subagent tree from hook events and derive
//! the FR-012 status, including the headline 0.5 state
//! [`SessionStatus::WaitingOnSubagents`].
//!
//! ## Inputs
//! Journal events (already durable, arriving from the agent over the spine):
//!   - `SubagentStart` / `SubagentStop` (each carries `agent_id`, `agent_type`)
//!     → child nodes under the owning `session_id`.
//!   - `TaskCreated` / `TaskCompleted` → an outstanding-task counter per session.
//!   - `UserPromptSubmit` → the orchestrator is `Working`.
//!   - `Stop` → **classify**: if `agent_id` children or tasks remain, the
//!     orchestrator is `WaitingOnSubagents`, *not* `Completed` (FR-012).
//!   - `Elicitation` → `NeedsQuestion`; `PermissionRequest` → `NeedsPermission`;
//!     `Notification` → `NeedsQuestion` (fallback); `StopFailure` /
//!     abnormal `SessionEnd` → `Failed`.
//!
//! ## Design
//! [`Supervisor`] is a pure in-memory reducer: feed it
//! `(session_id, agent_id?, JournalEventType, timestamp)` and it updates the
//! tree + status, returning the affected session id so the caller can emit a
//! fresh [`SupervisionTree`] snapshot to the UI. It is fully implemented +
//! tested here because the classification *is* the feature; the agent-bridge
//! wiring that feeds it lives in [`crate::agent`] (a separate seam).
//!
//! Boundary for parallel work: keep this module a deterministic reducer over
//! events with no I/O. The bridge/emit side belongs in `agent`/`claude`.

use std::collections::{HashMap, VecDeque};

use crate::model::{SessionStatus, SubagentNode, SubagentState, SupervisionTree};
use t_hub_protocol::JournalEventType;

/// Cap on the bounded transition log. Big enough that a poller sleeping 500ms
/// between checks cannot realistically miss an edge (each `ingest` pushes at
/// most one entry), small enough that the log stays trivially cheap.
const TRANSITION_LOG_CAP: usize = 256;

/// Hard cap on live session entries kept in [`Supervisor::sessions`]. Sessions are
/// normally evicted on their authoritative `SessionEnd` signal, but some sessions
/// never emit one (crash, kill -9, lost spine). Without a backstop the map would
/// grow one entry per Claude session id (a fresh UUID per spawn/resume) for the
/// life of the process — a slow RAM leak over a long-running hub. So we also bound
/// the map: once it exceeds this many entries, the least-recently-updated session
/// is evicted (LRU by the monotonic `update_stamp` below). 256 comfortably covers
/// every realistically-concurrent session while keeping growth hard-bounded.
const SESSION_MAP_CAP: usize = 256;

/// Cap on completed children retained per session. `SubagentStop` marks a child
/// `Completed` rather than dropping it so the UI can briefly render the finished
/// node; but a parallel-agent-heavy session (fresh `agent_id` per Task) would
/// otherwise accumulate completed nodes without bound. We keep at most this many
/// of the most-recently-finished completed children and prune the oldest beyond
/// it. Running children are never pruned (they're still outstanding). 64 keeps the
/// recently-finished tail visible while bounding the tree.
const COMPLETED_CHILDREN_CAP: usize = 64;

/// Per-session supervision state.
#[derive(Debug, Default, Clone)]
struct SessionEntry {
    /// Children keyed by `agent_id`.
    children: HashMap<String, SubagentNode>,
    /// Outstanding background tasks (`TaskCreated` − `TaskCompleted`, floored 0).
    outstanding_tasks: u32,
    /// Current derived status.
    status: SessionStatus,
    /// True once a `Stop` has fired on the main agent (so a later child finish
    /// can transition WaitingOnSubagents → Completed).
    main_stopped: bool,
    /// Monotonic "last touched" stamp (the `Supervisor.touch_seq` value at the
    /// last ingest for this session). Used purely as an LRU key for the
    /// [`SESSION_MAP_CAP`] backstop — NOT wall-clock, so it is immune to clock
    /// skew and deterministic in tests. Higher = more recently updated.
    update_stamp: u64,
}

impl SessionEntry {
    /// Count of children still running.
    fn running_children(&self) -> usize {
        self.children
            .values()
            .filter(|c| c.state == SubagentState::Running)
            .count()
    }

    /// True when nothing is outstanding (no running children, no open tasks).
    fn idle(&self) -> bool {
        self.running_children() == 0 && self.outstanding_tasks == 0
    }

    /// Bound the number of *completed* children to [`COMPLETED_CHILDREN_CAP`],
    /// dropping the oldest-finished ones first. Running children are never touched
    /// — they're still outstanding and drive the WaitingOnSubagents classification.
    /// This keeps a parallel-agent-heavy session (one fresh `agent_id` per Task)
    /// from accumulating completed nodes without bound while leaving the
    /// recently-finished tail visible to the UI. Cheap no-op until the cap is
    /// exceeded.
    fn prune_completed_children(&mut self) {
        let completed = self
            .children
            .values()
            .filter(|c| c.state == SubagentState::Completed)
            .count();
        if completed <= COMPLETED_CHILDREN_CAP {
            return;
        }
        // Evict the oldest-finished completed children (by `ended_at`, then
        // `agent_id` for a stable tie-break) until we're back at the cap. Running
        // children are excluded from the candidate set entirely.
        let mut finished: Vec<(Option<u64>, String)> = self
            .children
            .iter()
            .filter(|(_, c)| c.state == SubagentState::Completed)
            .map(|(id, c)| (c.ended_at, id.clone()))
            .collect();
        finished.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let to_remove = completed - COMPLETED_CHILDREN_CAP;
        for (_, id) in finished.into_iter().take(to_remove) {
            self.children.remove(&id);
        }
    }
}

/// The supervision reducer. One instance per core process; not `Sync` by itself
/// (the caller wraps it in a `Mutex`).
///
/// ## Edge-capturing transition log
/// The reducer is snapshot-only: `status()` reports the *current* value. A poller
/// (e.g. `control::wait_for_status`) that only reads `status()` between sleeps can
/// miss a status the session passed *through* (e.g. working→completed→working, or
/// a transient `NeedsQuestion`). To make those edges observable without a
/// subscription/condvar, every *actual* status change is appended to a bounded
/// [`VecDeque`] keyed by a monotonic `seq`. A poller captures `current_seq()` up
/// front, then asks `transitions_since`/`matched_since` for any edge it slept
/// through. The log is capped (oldest entries drop) so it never grows unbounded.
#[derive(Debug, Default)]
pub struct Supervisor {
    sessions: HashMap<String, SessionEntry>,
    /// Monotonic counter; the seq assigned to the *next* transition pushed. Also
    /// the `current_seq()` watermark a poller captures before it starts waiting.
    seq: u64,
    /// Bounded edge log of `(seq, session_id, new_status)`, oldest first. Capped
    /// at [`TRANSITION_LOG_CAP`]; pushing past the cap evicts the front.
    transitions: VecDeque<(u64, String, SessionStatus)>,
    /// Monotonic counter stamped onto a session's `update_stamp` on every ingest,
    /// giving each session an LRU recency key for the [`SESSION_MAP_CAP`] backstop.
    /// Independent of `seq` (which only advances on an *actual status change*): we
    /// want recency to reflect *any* touch, including same-status re-ingests, so a
    /// session that keeps emitting statusline/heartbeat events is never the LRU
    /// victim. Never wall-clock, so eviction order is deterministic + test-stable.
    touch_seq: u64,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event. `agent_id`/`agent_type` are taken from the hook's
    /// subagent base fields when present (`SubagentStart`/`SubagentStop`).
    /// Returns the affected `session_id` (so the caller can emit a snapshot), or
    /// `None` if the event carried no session id.
    pub fn ingest(
        &mut self,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        agent_type: Option<&str>,
        event: JournalEventType,
        timestamp_ms: u64,
    ) -> Option<String> {
        let session_id = session_id?;
        // Bump the monotonic touch counter and stamp it onto the entry below so the
        // session-map cap can evict by LRU. Done up front (any ingest counts as a
        // touch) so an actively-pinged session is never the eviction victim.
        self.touch_seq += 1;
        let touch = self.touch_seq;
        let entry = self.sessions.entry(session_id.to_string()).or_default();
        entry.update_stamp = touch;
        // Capture the status before the reducer runs so we can log an *edge* only
        // when it actually changes (not on a same-status re-ingest). `SessionStatus`
        // is `Copy`, so this is a cheap snapshot.
        let prev_status = entry.status;

        match event {
            JournalEventType::SessionStart => {
                entry.status = SessionStatus::Working;
                entry.main_stopped = false;
            }

            JournalEventType::UserPromptSubmit => {
                // A new turn began — back to Working regardless of prior state.
                entry.status = SessionStatus::Working;
                entry.main_stopped = false;
            }

            JournalEventType::SubagentStart => {
                if let Some(aid) = agent_id {
                    entry.children.entry(aid.to_string()).or_insert(SubagentNode {
                        parent_session_id: session_id.to_string(),
                        agent_id: aid.to_string(),
                        agent_type: agent_type.map(str::to_string),
                        state: SubagentState::Running,
                        started_at: timestamp_ms,
                        ended_at: None,
                    });
                }
                // Starting a subagent implies the orchestrator is actively working.
                if !entry.main_stopped {
                    entry.status = SessionStatus::Working;
                }
            }

            JournalEventType::SubagentStop => {
                if let Some(aid) = agent_id {
                    if let Some(child) = entry.children.get_mut(aid) {
                        child.state = SubagentState::Completed;
                        child.ended_at = Some(timestamp_ms);
                    }
                }
                // Bound the completed-children tail so a parallel-agent-heavy
                // session (fresh `agent_id` per Task) cannot accumulate finished
                // nodes without limit. Running children are kept (still
                // outstanding); only the oldest-finished beyond the cap are dropped.
                entry.prune_completed_children();
                // If the main agent already stopped and everything is now idle,
                // the orchestrator is finally Completed.
                Self::recompute_after_completion(entry);
            }

            JournalEventType::TaskCreated => {
                entry.outstanding_tasks = entry.outstanding_tasks.saturating_add(1);
                if !entry.main_stopped {
                    entry.status = SessionStatus::Working;
                }
            }

            JournalEventType::TaskCompleted => {
                entry.outstanding_tasks = entry.outstanding_tasks.saturating_sub(1);
                Self::recompute_after_completion(entry);
            }

            JournalEventType::Stop => {
                // The headline classification: a main-agent Stop with outstanding
                // children/tasks is WaitingOnSubagents, not Completed (FR-012).
                entry.main_stopped = true;
                entry.status = if entry.idle() {
                    SessionStatus::Completed
                } else {
                    SessionStatus::WaitingOnSubagents
                };
            }

            JournalEventType::Elicitation => {
                entry.status = SessionStatus::NeedsQuestion;
            }

            JournalEventType::PermissionRequest => {
                entry.status = SessionStatus::NeedsPermission;
            }

            JournalEventType::Notification => {
                // Fallback "needs input" signal; do not override a more specific
                // pending question/permission state.
                if !matches!(
                    entry.status,
                    SessionStatus::NeedsQuestion | SessionStatus::NeedsPermission
                ) {
                    entry.status = SessionStatus::NeedsQuestion;
                }
            }

            JournalEventType::StopFailure => {
                entry.status = SessionStatus::Failed;
            }

            JournalEventType::SessionEnd => {
                // Abnormal end → failed; a clean end after Completed stays
                // Completed. We can't always distinguish, so only downgrade to
                // Failed if we weren't already Completed.
                if entry.status != SessionStatus::Completed {
                    entry.status = SessionStatus::Failed;
                }
                // We KEEP the entry (with its terminal status) rather than evicting
                // here: the live-UI emit (`emit_session`) and a late `wait_for_status`
                // poller both read the current `status()` right after this ingest, so
                // an immediate eviction would make them see `Unknown` instead of the
                // real Completed/Failed end. The entry stops being touched, so it
                // becomes the least-recently-updated and the LRU cap below ages it
                // out first as new sessions arrive — bounded growth without losing
                // the terminal status.
            }

            // Events not relevant to the status reducer (cwd/worktree/status
            // snapshots/agent lifecycle) leave status unchanged here; the status
            // bridge handles rate-limit/context elsewhere.
            _ => {}
        }

        // Read the post-reducer status while the entry still exists, so we can log
        // the edge even when this event evicts the entry (e.g. `SessionEnd`). Log an
        // edge only on an *actual* change, so a same-status re-ingest does not
        // pollute the log.
        let new_status = self.sessions[session_id].status;
        if new_status != prev_status {
            self.push_transition(session_id, new_status);
        }

        // Keep the map hard-bounded by evicting the least-recently-updated entries
        // once it exceeds the cap. Ended sessions stop being touched, so they sort
        // oldest and age out first; the just-touched session (highest `update_stamp`)
        // is never the victim. Safe — every reader defaults an unseen session to
        // `Unknown` / `None`.
        self.enforce_session_cap();

        Some(session_id.to_string())
    }

    /// Bound [`Self::sessions`] to [`SESSION_MAP_CAP`] entries, evicting the
    /// least-recently-updated sessions (lowest `update_stamp`) until back at the
    /// cap. This is the backstop for the leak: a session that never emits
    /// `SessionEnd` would otherwise live forever. Recency is the monotonic
    /// `touch_seq` stamp (not wall-clock), so eviction order is deterministic. The
    /// just-touched session always has the max stamp and is never the victim. The
    /// transition log is untouched (it is independent of `sessions`).
    fn enforce_session_cap(&mut self) {
        if self.sessions.len() <= SESSION_MAP_CAP {
            return;
        }
        // Collect (update_stamp, id) and evict the oldest until at the cap. The map
        // only exceeds the cap by one per ingest, so this normally drops a single
        // entry; the loop is robust if the cap were ever lowered at runtime.
        let mut by_recency: Vec<(u64, String)> = self
            .sessions
            .iter()
            .map(|(id, e)| (e.update_stamp, id.clone()))
            .collect();
        by_recency.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let to_remove = self.sessions.len() - SESSION_MAP_CAP;
        for (_, id) in by_recency.into_iter().take(to_remove) {
            self.sessions.remove(&id);
        }
    }

    /// Append `(seq, session_id, status)` to the bounded transition log and bump
    /// `seq`. Seqs are 1-based and strictly increasing: we *pre*-increment so the
    /// first transition is seq 1, and a watermark captured as `current_seq()`
    /// (the highest seq assigned so far) plus an exclusive `> since` query catches
    /// every edge logged *after* the capture. Evicts the oldest entry once the cap
    /// is reached so the log stays `O(TRANSITION_LOG_CAP)`. Called only on an
    /// actual status change.
    fn push_transition(&mut self, session_id: &str, status: SessionStatus) {
        if self.transitions.len() >= TRANSITION_LOG_CAP {
            self.transitions.pop_front();
        }
        self.seq += 1;
        self.transitions
            .push_back((self.seq, session_id.to_string(), status));
    }

    /// The current transition watermark: the highest seq assigned so far (0 before
    /// any transition). A poller captures this before it starts waiting, then asks
    /// [`Self::transitions_since`] / [`Self::matched_since`] for edges with `seq >`
    /// this value — i.e. every transition logged *after* the capture, including a
    /// transient status the session passes through.
    pub fn current_seq(&self) -> u64 {
        self.seq
    }

    /// Every logged `(session_id, status)` transition with `seq > since_seq`, in
    /// order. Entries older than the cap are gone; a caller that captured
    /// `current_seq()` recently will see all edges since then.
    pub fn transitions_since(&self, since_seq: u64) -> Vec<(String, SessionStatus)> {
        self.transitions
            .iter()
            .filter(|(seq, _, _)| *seq > since_seq)
            .map(|(_, sid, status)| (sid.clone(), *status))
            .collect()
    }

    /// Focused edge query for one session: the *first* logged transition with
    /// `seq > since_seq` whose status is in `targets`, paired with the seq that
    /// matched (so the caller can advance its consumed watermark past it). Returns
    /// `None` if no such edge was logged.
    pub fn matched_since(
        &self,
        session_id: &str,
        targets: &[SessionStatus],
        since_seq: u64,
    ) -> Option<(u64, SessionStatus)> {
        self.transitions
            .iter()
            .find(|(seq, sid, status)| {
                *seq > since_seq && sid == session_id && targets.contains(status)
            })
            .map(|(seq, _, status)| (*seq, *status))
    }

    /// After a child/task finishes, if the main agent had already stopped and
    /// everything is now idle, transition WaitingOnSubagents → Completed.
    fn recompute_after_completion(entry: &mut SessionEntry) {
        if entry.main_stopped
            && entry.idle()
            && entry.status == SessionStatus::WaitingOnSubagents
        {
            entry.status = SessionStatus::Completed;
        }
    }

    /// Current status for a session (Unknown if unseen).
    pub fn status(&self, session_id: &str) -> SessionStatus {
        self.sessions
            .get(session_id)
            .map(|e| e.status)
            .unwrap_or(SessionStatus::Unknown)
    }

    /// Build the read-only tree snapshot for a session (PLAN.md §C tree view).
    /// Children are sorted by `started_at` then `agent_id` for stable rendering.
    pub fn tree(&self, session_id: &str) -> Option<SupervisionTree> {
        let entry = self.sessions.get(session_id)?;
        let mut children: Vec<SubagentNode> = entry.children.values().cloned().collect();
        children.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        });
        Some(SupervisionTree {
            session_id: session_id.to_string(),
            status: entry.status,
            children,
            outstanding_tasks: entry.outstanding_tasks,
        })
    }

    /// All known session ids (for snapshotting the whole tree on UI connect).
    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sup() -> Supervisor {
        Supervisor::new()
    }

    #[test]
    fn stop_with_running_subagent_is_waiting_not_completed() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 2);
        // Orchestrator spawns a subagent.
        s.ingest(
            Some("o1"),
            Some("a1"),
            Some("general-purpose"),
            JournalEventType::SubagentStart,
            3,
        );
        // Main agent's Stop fires while the child is still running.
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 4);
        assert_eq!(
            s.status("o1"),
            SessionStatus::WaitingOnSubagents,
            "Stop with a running subagent must be WaitingOnSubagents (FR-012)"
        );

        // The child finishes → orchestrator transitions to Completed.
        s.ingest(Some("o1"), Some("a1"), None, JournalEventType::SubagentStop, 5);
        assert_eq!(s.status("o1"), SessionStatus::Completed);
    }

    #[test]
    fn stop_with_outstanding_task_is_waiting() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::TaskCreated, 2);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 3);
        assert_eq!(s.status("o1"), SessionStatus::WaitingOnSubagents);
        s.ingest(Some("o1"), None, None, JournalEventType::TaskCompleted, 4);
        assert_eq!(s.status("o1"), SessionStatus::Completed);
    }

    #[test]
    fn stop_with_nothing_outstanding_is_completed() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 2);
        assert_eq!(s.status("o1"), SessionStatus::Completed);
    }

    #[test]
    fn elicitation_and_permission_map_to_states() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::Elicitation, 2);
        assert_eq!(s.status("o1"), SessionStatus::NeedsQuestion);
        s.ingest(Some("o1"), None, None, JournalEventType::PermissionRequest, 3);
        assert_eq!(s.status("o1"), SessionStatus::NeedsPermission);
    }

    #[test]
    fn tree_reports_children_and_task_count() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), Some("a1"), Some("explore"), JournalEventType::SubagentStart, 2);
        s.ingest(Some("o1"), Some("a2"), Some("plan"), JournalEventType::SubagentStart, 3);
        s.ingest(Some("o1"), None, None, JournalEventType::TaskCreated, 4);
        s.ingest(Some("o1"), Some("a1"), None, JournalEventType::SubagentStop, 5);

        let tree = s.tree("o1").unwrap();
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.outstanding_tasks, 1);
        // a1 started before a2 → stable order; a1 completed, a2 running.
        assert_eq!(tree.children[0].agent_id, "a1");
        assert_eq!(tree.children[0].state, SubagentState::Completed);
        assert_eq!(tree.children[1].agent_id, "a2");
        assert_eq!(tree.children[1].state, SubagentState::Running);
    }

    #[test]
    fn new_turn_after_completion_returns_to_working() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 2);
        assert_eq!(s.status("o1"), SessionStatus::Completed);
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 3);
        assert_eq!(s.status("o1"), SessionStatus::Working);
    }

    #[test]
    fn stop_failure_is_failed() {
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::StopFailure, 2);
        assert_eq!(s.status("o1"), SessionStatus::Failed);
    }

    #[test]
    fn transition_log_captures_transient_edge_through_b() {
        // Drive A(Working) → B(Completed, transient) → A(Working). The current
        // status ends back at A, but the edge through B must be logged so a poller
        // that captured `start` before the run can still observe it after the fact.
        let mut s = sup();
        let start = s.current_seq();
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 1);
        assert_eq!(s.status("o1"), SessionStatus::Working);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 2);
        assert_eq!(s.status("o1"), SessionStatus::Completed);
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 3);
        // Current status is back to Working — Completed was only transient.
        assert_eq!(s.status("o1"), SessionStatus::Working);

        // The transient Completed edge is still recoverable from the log.
        let edges = s.transitions_since(start);
        assert!(
            edges
                .iter()
                .any(|(sid, st)| sid == "o1" && *st == SessionStatus::Completed),
            "expected a logged Completed edge in {edges:?}"
        );
        // Focused query finds it too, and reports the seq that matched.
        let matched = s.matched_since("o1", &[SessionStatus::Completed], start);
        assert!(
            matches!(matched, Some((_, SessionStatus::Completed))),
            "matched_since should find the transient Completed edge, got {matched:?}"
        );
    }

    #[test]
    fn transition_log_only_records_actual_changes() {
        // Re-ingesting the same-status signal must not push a duplicate edge.
        let mut s = sup();
        let start = s.current_seq();
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 1);
        // A second UserPromptSubmit while already Working is a same-status re-ingest.
        s.ingest(Some("o1"), None, None, JournalEventType::UserPromptSubmit, 2);
        let working_edges = s
            .transitions_since(start)
            .into_iter()
            .filter(|(sid, st)| sid == "o1" && *st == SessionStatus::Working)
            .count();
        assert_eq!(working_edges, 1, "same-status re-ingest must not log an edge");
    }

    // --- Memory-leak fixes: eviction + bounded growth --------------------------

    #[test]
    fn session_end_keeps_entry_with_terminal_status() {
        // `SessionEnd` must KEEP the entry (with its terminal status) so the live-UI
        // emit + a late `wait_for_status` poller read the real end status, not
        // Unknown. Bounded growth is the LRU cap's job (the entry stops being touched
        // → ages out oldest-first), not an immediate evict here.
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), Some("a1"), Some("explore"), JournalEventType::SubagentStart, 2);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 3);
        assert!(s.tree("o1").is_some());

        s.ingest(Some("o1"), None, None, JournalEventType::SessionEnd, 4);
        // Entry survives with the terminal status (Failed — it wasn't Completed).
        assert!(s.tree("o1").is_some(), "tree must survive SessionEnd for the UI");
        assert_eq!(s.session_ids().len(), 1);
        assert_eq!(
            s.status("o1"),
            SessionStatus::Failed,
            "ended session keeps its terminal status (not Unknown)"
        );
    }

    #[test]
    fn session_end_after_completed_keeps_completed_status() {
        // A clean Completed → SessionEnd must stay Completed and KEEP the entry, so a
        // `wait_for_status(completed)` poller that polls right after the end still
        // sees Completed instead of a spurious timeout. (Regression guard.)
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::Stop, 2); // no children → Completed
        assert_eq!(s.status("o1"), SessionStatus::Completed);
        s.ingest(Some("o1"), None, None, JournalEventType::SessionEnd, 3);
        assert_eq!(
            s.status("o1"),
            SessionStatus::Completed,
            "clean end stays Completed and is still readable"
        );
    }

    #[test]
    fn session_end_logs_terminal_transition() {
        // The terminal transition (here Working→Failed on an abnormal end) is logged,
        // so a poller that captured `start` before the run observes it via the log.
        let mut s = sup();
        let start = s.current_seq();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("o1"), None, None, JournalEventType::SessionEnd, 2);
        let matched = s.matched_since("o1", &[SessionStatus::Failed], start);
        assert!(
            matches!(matched, Some((_, SessionStatus::Failed))),
            "terminal Failed edge must be logged, got {matched:?}"
        );
    }

    #[test]
    fn completed_children_are_capped_running_kept() {
        // A parallel-agent-heavy session finishes far more children than the cap.
        // Completed children beyond the cap are pruned oldest-first; a still-running
        // child is always retained (it's outstanding and drives WaitingOnSubagents).
        let mut s = sup();
        s.ingest(Some("o1"), None, None, JournalEventType::SessionStart, 1);
        // One long-running child that never stops.
        s.ingest(Some("o1"), Some("run"), Some("worker"), JournalEventType::SubagentStart, 2);

        // Finish far more than the cap's worth of children.
        let n = COMPLETED_CHILDREN_CAP + 50;
        for i in 0..n {
            let aid = format!("c{i:04}");
            let start_ts = 100 + i as u64;
            s.ingest(Some("o1"), Some(&aid), Some("worker"), JournalEventType::SubagentStart, start_ts);
            // End them in order so `ended_at` reflects finish order.
            s.ingest(Some("o1"), Some(&aid), None, JournalEventType::SubagentStop, start_ts + 1);
        }

        let tree = s.tree("o1").unwrap();
        let completed = tree
            .children
            .iter()
            .filter(|c| c.state == SubagentState::Completed)
            .count();
        let running = tree
            .children
            .iter()
            .filter(|c| c.state == SubagentState::Running)
            .count();
        assert_eq!(completed, COMPLETED_CHILDREN_CAP, "completed children are capped");
        assert_eq!(running, 1, "the still-running child is never pruned");

        // The oldest-finished children were the victims: c0000 is gone, the most
        // recent completed ones survive.
        assert!(
            tree.children.iter().all(|c| c.agent_id != "c0000"),
            "oldest-finished completed child must be pruned"
        );
        let last = format!("c{:04}", n - 1);
        assert!(
            tree.children.iter().any(|c| c.agent_id == last),
            "most-recently-finished completed child must survive"
        );
    }

    #[test]
    fn session_map_cap_evicts_oldest_when_no_session_end() {
        // Sessions that never emit SessionEnd must still be hard-bounded: once the
        // map exceeds the cap, the least-recently-updated session is evicted.
        let mut s = sup();
        // Fill exactly to the cap; each session is touched once, in order, so their
        // `update_stamp`s ascend s0 < s1 < ... .
        for i in 0..SESSION_MAP_CAP {
            let sid = format!("s{i:05}");
            s.ingest(Some(&sid), None, None, JournalEventType::SessionStart, i as u64);
        }
        assert_eq!(s.session_ids().len(), SESSION_MAP_CAP, "filled to the cap");

        // Re-touch the oldest session so it is NO LONGER the LRU victim — proving
        // eviction is recency-based, not insertion-based.
        s.ingest(Some("s00000"), None, None, JournalEventType::UserPromptSubmit, 9_000);

        // One more brand-new session pushes us over the cap → exactly one eviction.
        s.ingest(Some("s99999"), None, None, JournalEventType::SessionStart, 9_001);
        assert_eq!(
            s.session_ids().len(),
            SESSION_MAP_CAP,
            "map stays hard-bounded at the cap"
        );
        // The re-touched oldest survives; the now-least-recent (s00001) is gone.
        assert_eq!(s.status("s00000"), SessionStatus::Working, "re-touched session survives");
        assert_eq!(
            s.status("s00001"),
            SessionStatus::Unknown,
            "least-recently-updated session was evicted"
        );
        assert_eq!(s.status("s99999"), SessionStatus::Working, "the new session is present");
    }
}
