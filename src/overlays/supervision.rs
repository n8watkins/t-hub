//! Supervision overlay (T9): the orchestrator -> subagent tree with per-session
//! status.
//!
//! Data: seeded once from `supervision_session_ids` + `supervision_tree` (the
//! webview's mount pull), then live off `supervision://tree`, `session://status`
//! and `agent://title` events. Keys are Claude session UUIDs (NOT tmux ids -
//! §1.2's two id spaces).

use std::collections::HashMap;

use serde::Deserialize;

use super::model::{fmt_duration, SessionStatus};

/// Per-subagent state (`model.rs SubagentState`, camelCase). An unknown future
/// variant folds to `Running` - the safe display for a live-ish state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentState {
    Completed,
    #[serde(other)]
    Running,
}

/// One child node (`model.rs SubagentNode`, camelCase).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNode {
    pub parent_session_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub agent_type: Option<String>,
    pub state: SubagentState,
    #[serde(default)]
    pub started_at: u64, // epoch ms
    #[serde(default)]
    pub ended_at: Option<u64>, // epoch ms, None while running
}

/// One orchestrator's tree (`model.rs SupervisionTree`, camelCase). This is both
/// the `supervision_tree` command result and the `supervision://tree` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisionTree {
    pub session_id: String,
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub children: Vec<SubagentNode>,
    #[serde(default)]
    pub outstanding_tasks: u32,
}

/// Child row view-model.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildView {
    /// `agent_type` when known, else the first 8 chars of the agent id.
    pub label: String,
    pub running: bool,
    /// Elapsed label for finished children ("42s", "3m10s").
    pub duration: Option<String>,
}

/// Tree block view-model: header badge + counts line + child rows.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeView {
    pub session_id: String,
    /// Claude-derived session title when known, else a shortened session id.
    pub label: String,
    pub status: SessionStatus,
    pub running: usize,
    pub done: usize,
    pub outstanding_tasks: u32,
    pub children: Vec<ChildView>,
}

/// Supervision state: trees + statuses + titles, keyed by session UUID.
#[derive(Debug, Default)]
pub struct SupervisionState {
    trees: HashMap<String, SupervisionTree>,
    statuses: HashMap<String, SessionStatus>,
    titles: HashMap<String, String>,
    last_activity_ms: HashMap<String, u64>,
}

impl SupervisionState {
    /// Fold a tree (command seed or `supervision://tree` event). Mirrors the
    /// status like the webview store does.
    pub fn fold_tree(&mut self, tree: SupervisionTree, now_ms: u64) {
        self.statuses.insert(tree.session_id.clone(), tree.status);
        self.last_activity_ms.insert(tree.session_id.clone(), now_ms);
        self.trees.insert(tree.session_id.clone(), tree);
    }

    /// Fold a `session://status` event.
    pub fn fold_status(&mut self, session_id: &str, status: SessionStatus, now_ms: u64) {
        self.statuses.insert(session_id.to_string(), status);
        self.last_activity_ms.insert(session_id.to_string(), now_ms);
        if let Some(t) = self.trees.get_mut(session_id) {
            t.status = status;
        }
    }

    /// Fold an `agent://title` event (used as the tree block label).
    pub fn fold_title(&mut self, session_id: &str, title: &str) {
        self.titles.insert(session_id.to_string(), title.to_string());
    }

    pub fn status_of(&self, session_id: &str) -> SessionStatus {
        self.statuses.get(session_id).copied().unwrap_or_default()
    }

    /// The tree blocks to render: only orchestrators with subagent activity
    /// (children or outstanding tasks), live ones first, then by most recent
    /// activity. The webview renders one tree per session detail view; a sidebar
    /// shows every active orchestrator at once.
    pub fn active(&self) -> Vec<TreeView> {
        let mut views: Vec<(bool, u64, TreeView)> = self
            .trees
            .values()
            .filter(|t| !t.children.is_empty() || t.outstanding_tasks > 0)
            .map(|t| {
                let running =
                    t.children.iter().filter(|c| c.state == SubagentState::Running).count();
                let done = t.children.len() - running;
                let label = match self.titles.get(&t.session_id) {
                    Some(title) => title.clone(),
                    None => short_id(&t.session_id),
                };
                let children = t
                    .children
                    .iter()
                    .map(|c| ChildView {
                        label: c
                            .agent_type
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| short_id(&c.agent_id)),
                        running: c.state == SubagentState::Running,
                        duration: c
                            .ended_at
                            .map(|end| fmt_duration(end.saturating_sub(c.started_at))),
                    })
                    .collect();
                let is_live = running > 0 || t.outstanding_tasks > 0;
                let seen = self.last_activity_ms.get(&t.session_id).copied().unwrap_or(0);
                (
                    is_live,
                    seen,
                    TreeView {
                        session_id: t.session_id.clone(),
                        label,
                        status: self.status_of(&t.session_id),
                        running,
                        done,
                        outstanding_tasks: t.outstanding_tasks,
                        children,
                    },
                )
            })
            .collect();
        views.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        views.into_iter().map(|(_, _, v)| v).collect()
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(id: &str, status: SessionStatus, children: Vec<SubagentNode>, tasks: u32) -> SupervisionTree {
        SupervisionTree {
            session_id: id.to_string(),
            status,
            children,
            outstanding_tasks: tasks,
        }
    }

    fn child(agent_id: &str, agent_type: Option<&str>, state: SubagentState, started: u64, ended: Option<u64>) -> SubagentNode {
        SubagentNode {
            parent_session_id: "p".to_string(),
            agent_id: agent_id.to_string(),
            agent_type: agent_type.map(|s| s.to_string()),
            state,
            started_at: started,
            ended_at: ended,
        }
    }

    #[test]
    fn parses_the_wire_shape() {
        let v = serde_json::json!({
            "sessionId": "uuid-1",
            "status": "waitingOnSubagents",
            "children": [{
                "parentSessionId": "uuid-1",
                "agentId": "agent-42",
                "agentType": "Explore",
                "state": "running",
                "startedAt": 1_750_000_000_000u64
            }],
            "outstandingTasks": 2
        });
        let t: SupervisionTree = serde_json::from_value(v).unwrap();
        assert_eq!(t.status, SessionStatus::WaitingOnSubagents);
        assert_eq!(t.children[0].state, SubagentState::Running);
        assert_eq!(t.outstanding_tasks, 2);
    }

    #[test]
    fn fold_tree_mirrors_status_and_fold_status_updates_it() {
        let mut st = SupervisionState::default();
        st.fold_tree(tree("a", SessionStatus::Working, vec![], 1), 100);
        assert_eq!(st.status_of("a"), SessionStatus::Working);
        st.fold_status("a", SessionStatus::Completed, 200);
        assert_eq!(st.status_of("a"), SessionStatus::Completed);
        assert_eq!(st.active()[0].status, SessionStatus::Completed);
    }

    #[test]
    fn active_skips_idle_trees_and_orders_live_first_then_recent() {
        let mut st = SupervisionState::default();
        // No children, no tasks: not shown.
        st.fold_tree(tree("idle", SessionStatus::Working, vec![], 0), 400);
        // Finished children only, older activity.
        st.fold_tree(
            tree("done-old", SessionStatus::Completed,
                vec![child("x", None, SubagentState::Completed, 0, Some(1000))], 0),
            100,
        );
        // Finished children only, newer activity.
        st.fold_tree(
            tree("done-new", SessionStatus::Completed,
                vec![child("y", None, SubagentState::Completed, 0, Some(1000))], 0),
            300,
        );
        // A running child: live, sorts first despite oldest activity.
        st.fold_tree(
            tree("live", SessionStatus::WaitingOnSubagents,
                vec![child("z", None, SubagentState::Running, 0, None)], 0),
            50,
        );
        let ids: Vec<String> = st.active().into_iter().map(|v| v.session_id).collect();
        assert_eq!(ids, vec!["live", "done-new", "done-old"]);
    }

    #[test]
    fn child_view_labels_counts_and_durations() {
        let mut st = SupervisionState::default();
        st.fold_tree(
            tree("a", SessionStatus::WaitingOnSubagents,
                vec![
                    child("agent-12345678-rest", Some("Explore"), SubagentState::Running, 5000, None),
                    child("agent-abcdefgh-rest", None, SubagentState::Completed, 1000, Some(43_000)),
                ],
                2),
            100,
        );
        let v = &st.active()[0];
        assert_eq!((v.running, v.done, v.outstanding_tasks), (1, 1, 2));
        assert_eq!(v.children[0].label, "Explore");
        assert!(v.children[0].running);
        assert_eq!(v.children[0].duration, None);
        assert_eq!(v.children[1].label, "agent-ab");
        assert_eq!(v.children[1].duration.as_deref(), Some("42s"));
    }

    #[test]
    fn title_event_relabels_the_tree() {
        let mut st = SupervisionState::default();
        st.fold_tree(
            tree("uuid-abcdef12", SessionStatus::Working,
                vec![child("c", None, SubagentState::Running, 0, None)], 0),
            1,
        );
        assert_eq!(st.active()[0].label, "uuid-abc");
        st.fold_title("uuid-abcdef12", "Porting the sidebar");
        assert_eq!(st.active()[0].label, "Porting the sidebar");
    }
}
