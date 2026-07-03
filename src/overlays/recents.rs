//! Recents overlay (T9): recent resumable Claude sessions + the resume flow.
//!
//! Data: the `recent_sessions` control command (`recent.rs RecentSession[]`,
//! camelCase). State logic mirrors the webview's `RecentList.tsx`: newest-first,
//! one row per cwd, rows for currently-open projects filtered out, local
//! optimistic hide backed by the `archive_recent_project` command, and a 1.5s
//! resume gate against double-spawns.

use std::collections::HashSet;

use serde::Deserialize;

use super::model::{cwd_basename, rel_time};

/// How long the resume affordance stays disabled after a click (webview parity).
pub const RESUME_GATE_MS: u64 = 1500;

/// One recallable session (`recent.rs RecentSession`, camelCase on the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    /// Claude's session id - the `claude --resume <id>` handle.
    pub id: String,
    /// Working directory (WSL path); resume spawns here.
    pub cwd: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub last_text: String,
    /// Unix epoch SECONDS of last activity.
    #[serde(default)]
    pub last_seen: i64,
}

/// Row view-model: everything the render fn paints, precomputed as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentRow {
    pub id: String,
    pub cwd: String,
    /// Claude's summary label, or the cwd basename when absent.
    pub title: String,
    pub folder: String,
    /// Worktree hint (`wt-<branch>` segment or `.claude/worktrees/<name>`).
    pub worktree: Option<String>,
    /// Relative age label ("3m", "2h", ...).
    pub age: String,
    pub last_text: String,
}

/// Recents state: fed by `recent_sessions` fetches, folded by the reducers below.
#[derive(Debug, Default)]
pub struct RecentsState {
    /// Newest-first, already deduped to one entry per cwd.
    entries: Vec<RecentEntry>,
    /// Locally dismissed cwds (optimistic; `archive_recent_project` is the
    /// durable half, handled by the feed).
    hidden: HashSet<String>,
    /// First fetch landed (drives the "Loading..." empty state).
    pub loaded: bool,
    /// Last fetch error, shown dim when the list is otherwise empty.
    pub error: Option<String>,
    /// Resume clicks are blocked until this instant (double-spawn gate).
    resume_blocked_until_ms: u64,
}

impl RecentsState {
    /// Fold a `recent_sessions` result: sort newest-first and keep one entry per
    /// cwd (the newest). The server already caps to one session per project; the
    /// dedup here is defensive, like the webview's.
    pub fn fold_list(&mut self, mut list: Vec<RecentEntry>) {
        list.sort_by_key(|e| std::cmp::Reverse(e.last_seen));
        let mut seen = HashSet::new();
        self.entries = list
            .into_iter()
            .filter(|e| seen.insert(norm_cwd(&e.cwd).to_string()))
            .collect();
        self.loaded = true;
        self.error = None;
    }

    pub fn fold_error(&mut self, err: String) {
        self.error = Some(err);
        self.loaded = true;
    }

    /// Optimistically hide a project row. The feed follows up with the durable
    /// `archive_recent_project` request + a refresh.
    pub fn hide(&mut self, cwd: &str) {
        self.hidden.insert(norm_cwd(cwd).to_string());
    }

    /// Gate a resume click: returns `true` (and arms the gate) if allowed, or
    /// `false` when a resume fired within the last [`RESUME_GATE_MS`].
    pub fn begin_resume(&mut self, now_ms: u64) -> bool {
        if now_ms < self.resume_blocked_until_ms {
            return false;
        }
        self.resume_blocked_until_ms = now_ms + RESUME_GATE_MS;
        true
    }

    /// The rows to render: hidden rows and rows whose project is already open
    /// (cwd matches a live session's cwd) are filtered out - they re-appear when
    /// the project closes, exactly like the webview.
    pub fn rows(&self, open_cwds: &HashSet<String>, now_ms: u64) -> Vec<RecentRow> {
        self.entries
            .iter()
            .filter(|e| {
                let c = norm_cwd(&e.cwd);
                !self.hidden.contains(c) && !open_cwds.contains(c)
            })
            .map(|e| {
                let folder = cwd_basename(&e.cwd).to_string();
                let title =
                    if e.label.trim().is_empty() { folder.clone() } else { e.label.clone() };
                RecentRow {
                    id: e.id.clone(),
                    cwd: e.cwd.clone(),
                    title,
                    folder,
                    worktree: worktree_hint(&e.cwd),
                    age: rel_time(now_ms, e.last_seen),
                    last_text: e.last_text.clone(),
                }
            })
            .collect()
    }
}

/// Normalize a cwd for identity comparisons (trailing-slash tolerant).
pub fn norm_cwd(cwd: &str) -> &str {
    let t = cwd.trim_end_matches('/');
    if t.is_empty() { "/" } else { t }
}

/// The startup command a resume click sends through the socket `spawn_terminal`
/// (T-B). Mirrors the webview's `recall` exactly (`workspace.ts`): the id is
/// single-quoted as a defensive guard — ids are plain UUIDs, but the command
/// line passes through a shell inside the server's login-shell wrap.
pub fn resume_command(session_id: &str) -> String {
    format!("claude --resume '{}'", session_id.trim())
}

/// Derive a worktree hint from a cwd, mirroring the webview's FolderGroup logic:
/// a path segment starting with `wt-` (branch worktrees), or the segment after
/// `.claude/worktrees/` (task worktrees). `None` when the path looks like a
/// plain checkout.
pub fn worktree_hint(cwd: &str) -> Option<String> {
    let segs: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    for (i, s) in segs.iter().enumerate() {
        if let Some(rest) = s.strip_prefix("wt-") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
        if *s == "worktrees" && i > 0 && segs[i - 1] == ".claude" {
            if let Some(name) = segs.get(i + 1) {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, cwd: &str, last_seen: i64) -> RecentEntry {
        RecentEntry {
            id: id.to_string(),
            cwd: cwd.to_string(),
            label: String::new(),
            last_text: String::new(),
            last_seen,
        }
    }

    #[test]
    fn parses_the_wire_shape() {
        let v = serde_json::json!([{
            "id": "sess-1",
            "cwd": "/home/n/projects/app",
            "label": "Fixing the sidebar",
            "lastText": "done",
            "lastSeen": 1750000000i64
        }]);
        let list: Vec<RecentEntry> = serde_json::from_value(v).unwrap();
        assert_eq!(list[0].label, "Fixing the sidebar");
        assert_eq!(list[0].last_seen, 1_750_000_000);
    }

    #[test]
    fn fold_list_sorts_newest_first_and_dedupes_by_cwd() {
        let mut st = RecentsState::default();
        st.fold_list(vec![
            entry("old", "/p/a", 100),
            entry("new", "/p/a/", 200), // same project, trailing slash
            entry("b", "/p/b", 150),
        ]);
        let rows = st.rows(&HashSet::new(), 300_000);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "new"); // newest first, newest wins the cwd
        assert_eq!(rows[1].id, "b");
        assert!(st.loaded);
    }

    #[test]
    fn rows_filter_hidden_and_open_projects() {
        let mut st = RecentsState::default();
        st.fold_list(vec![entry("a", "/p/a", 3), entry("b", "/p/b", 2), entry("c", "/p/c", 1)]);
        st.hide("/p/a/"); // trailing slash must still match
        let open: HashSet<String> = ["/p/b".to_string()].into();
        let rows = st.rows(&open, 10_000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "c");
    }

    #[test]
    fn title_falls_back_to_the_folder_name() {
        let mut st = RecentsState::default();
        let mut e = entry("a", "/home/n/projects/t-hub", 1);
        e.label = "  ".to_string();
        st.fold_list(vec![e]);
        let rows = st.rows(&HashSet::new(), 10_000);
        assert_eq!(rows[0].title, "t-hub");
        assert_eq!(rows[0].folder, "t-hub");
    }

    #[test]
    fn resume_gate_blocks_within_the_window() {
        let mut st = RecentsState::default();
        assert!(st.begin_resume(10_000));
        assert!(!st.begin_resume(10_000 + RESUME_GATE_MS - 1));
        assert!(st.begin_resume(10_000 + RESUME_GATE_MS));
    }

    #[test]
    fn resume_command_quotes_the_id() {
        assert_eq!(
            resume_command("0197-abc "),
            "claude --resume '0197-abc'"
        );
    }

    #[test]
    fn worktree_hints() {
        assert_eq!(worktree_hint("/home/n/p/wt-feature-x"), Some("feature-x".to_string()));
        assert_eq!(
            worktree_hint("/home/n/p/app/.claude/worktrees/t9-overlays"),
            Some("t9-overlays".to_string())
        );
        assert_eq!(worktree_hint("/home/n/p/app"), None);
        // a bare "worktrees" dir without .claude parent is not a hint
        assert_eq!(worktree_hint("/home/n/worktrees"), None);
    }
}
