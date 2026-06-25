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
        let entry = self.sessions.entry(session_id.to_string()).or_default();
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
            }

            // Events not relevant to the status reducer (cwd/worktree/status
            // snapshots/agent lifecycle) leave status unchanged here; the status
            // bridge handles rate-limit/context elsewhere.
            _ => {}
        }

        // Read the post-reducer status (the `entry` borrow above ended with the
        // match). Log an edge only on an *actual* change, so a same-status
        // re-ingest does not pollute the log.
        let new_status = self.sessions[session_id].status;
        if new_status != prev_status {
            self.push_transition(session_id, new_status);
        }

        Some(session_id.to_string())
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
}
