//! Panels (T11): Files (tree + fuzzy search), Preview (local dev URLs) and
//! Dev runner - native counterparts of the webview's FilePanel / WebPreview /
//! DevTab, usable as a native tile or side surface.
//!
//! Layering follows the T9 overlays template exactly:
//!  - one gpui-free module per panel ([`files`], [`preview`], [`runner`]) -
//!    wire payload parsing + a state struct + reducers + plain-data
//!    view-models, all unit-testable under `--no-default-features`;
//!  - [`PanelsState`] composes the three plus the shared project list;
//!  - [`feed::PanelsFeed`] owns ALL ControlClient I/O (command polls + UI
//!    actions) on background threads. It subscribes to NO events - the wire's
//!    event channel is competing-consumer and the T9 `OverlayFeed` must stay
//!    the process's single drainer - so panels compose safely beside the
//!    overlays in one process;
//!  - [`view::PanelHost`] (feature `gui`) is the exported composite element
//!    the host mounts (see its mount contract), with `panel-window` as the
//!    standalone demo bin.
//!
//! Data sources (read tier unless noted): `list_terminals`, `list_dir`,
//! `index_project`, `search_files`, `open_file`, `git_info`, `read_terminal`;
//! the dev runner additionally drives the audited process-tier
//! `spawn_terminal` / `send_text` / `send_keys` / `close_terminal` (see
//! [`runner`] for why and for the safety handshake). Arbitrary-path file
//! WRITE stays M4-gated server-side; no such command exists or is added.

pub mod feed;
pub mod files;
pub mod preview;
pub mod probe;
pub mod runner;

#[cfg(feature = "gui")]
pub mod view;

#[cfg(feature = "gui")]
pub use view::PanelHost;

pub use feed::{PanelAction, PanelsFeed};

use files::{FetchDir, FilesState};
use preview::PreviewState;
use runner::RunnersState;

/// Wall-clock ms (same helper the overlays carry; local so `panels` does not
/// lean on `overlays` internals).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One live session from `list_terminals` (the fields panels care about).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    /// tmux-derived id (`th_`-stripped 8-hex).
    pub id: String,
    pub title: String,
    /// The pane's live cwd ("" when pane_info failed server-side).
    pub cwd: String,
}

/// A project root derived from the live sessions' cwds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: String,
    /// Directory basename, for the picker.
    pub name: String,
    /// How many live sessions sit in this root.
    pub sessions: usize,
}

/// Everything the panels render. Owned behind `Arc<Mutex<..>>` by
/// [`feed::PanelsFeed`]; written by its background threads, read by the
/// render pass each frame.
#[derive(Debug, Default)]
pub struct PanelsState {
    pub files: FilesState,
    pub preview: PreviewState,
    pub runners: RunnersState,
    /// Distinct project roots, sorted by name (ties by path).
    pub projects: Vec<Project>,
    /// The root the Files + Run tabs operate on.
    pub selected_root: Option<String>,
    pub live: Vec<LiveSession>,
    /// Last `list_terminals` failure, for the empty-state hint.
    pub error: Option<String>,
}

impl PanelsState {
    pub fn new() -> Self {
        PanelsState { files: FilesState::new(), ..Default::default() }
    }

    /// Fold a `list_terminals` sweep: rebuilds the project list, updates the
    /// preview session metadata, and auto-selects the first project when
    /// nothing is selected yet. Returns directory fetches the feed must run
    /// (from an auto-selection). The caller must ALSO fold
    /// `runners.on_sessions` and execute its commands.
    pub fn fold_live_sessions(&mut self, live: Vec<LiveSession>) -> Vec<FetchDir> {
        self.error = None;
        let mut projects: Vec<Project> = Vec::new();
        for s in &live {
            if s.cwd.is_empty() {
                continue;
            }
            let root = s.cwd.trim_end_matches('/').to_string();
            let root = if root.is_empty() { "/".to_string() } else { root };
            match projects.iter_mut().find(|p| p.root == root) {
                Some(p) => p.sessions += 1,
                None => projects.push(Project {
                    name: root.rsplit('/').next().unwrap_or(&root).to_string(),
                    root,
                    sessions: 1,
                }),
            }
        }
        projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
        self.projects = projects;

        let meta: Vec<(String, String, String)> = live
            .iter()
            .map(|s| (s.id.clone(), s.title.clone(), s.cwd.clone()))
            .collect();
        self.preview.fold_sessions(&meta);
        // NOTE: the caller (feed/probe) folds `runners.on_sessions(&self.live)`
        // itself - its returned RunnerCmds are socket work only the caller can
        // execute, so it must not be swallowed here.
        self.live = live;

        // A selection is STICKY once made: live pane cwds churn as agents cd
        // around (a project can vanish from this list while its directory is
        // perfectly valid), and a host-bound root (feed.set_root) may never
        // appear in it at all. Only the initial auto-select comes from here.
        if self.selected_root.is_none() {
            if let Some(root) = self.projects.first().map(|p| p.root.clone()) {
                return self.select_root(&root);
            }
        }
        Vec::new()
    }

    /// Select a project root (Files tree + Run target). Returns fetches for
    /// the feed (the root listing, when not yet cached).
    pub fn select_root(&mut self, root: &str) -> Vec<FetchDir> {
        if self.selected_root.as_deref() == Some(root) {
            return Vec::new();
        }
        self.selected_root = Some(root.to_string());
        self.files.set_root(root).into_iter().collect()
    }

    /// Cycle the selected project by `delta` in picker order.
    pub fn cycle_project(&mut self, delta: isize) -> Vec<FetchDir> {
        if self.projects.is_empty() {
            return Vec::new();
        }
        let cur = self
            .selected_root
            .as_ref()
            .and_then(|r| self.projects.iter().position(|p| &p.root == r))
            .unwrap_or(0);
        let n = self.projects.len() as isize;
        let next = ((cur as isize + delta) % n + n) % n;
        let root = self.projects[next as usize].root.clone();
        self.select_root(&root)
    }

    /// The runner for the selected project, if one has been created.
    pub fn selected_runner(&self) -> Option<&runner::RunnerState> {
        self.runners.get(self.selected_root.as_deref()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(id: &str, cwd: &str) -> LiveSession {
        LiveSession { id: id.into(), title: format!("th_{id}"), cwd: cwd.into() }
    }

    #[test]
    fn projects_fold_dedups_counts_and_sorts() {
        let mut st = PanelsState::new();
        let fetches = st.fold_live_sessions(
            vec![
                ls("a", "/home/u/zebra"),
                ls("b", "/home/u/apple/"),
                ls("c", "/home/u/apple"),
                ls("d", ""), // pane_info failure: skipped
            ],
        );
        assert_eq!(st.projects.len(), 2);
        assert_eq!(st.projects[0].name, "apple");
        assert_eq!(st.projects[0].sessions, 2, "trailing slash folded");
        assert_eq!(st.projects[1].name, "zebra");
        // Auto-selected the first project and requested its root listing.
        assert_eq!(st.selected_root.as_deref(), Some("/home/u/apple"));
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].path, "/home/u/apple");
    }

    #[test]
    fn selection_is_sticky_across_project_churn() {
        let mut st = PanelsState::new();
        st.fold_live_sessions(vec![ls("a", "/p/alpha"), ls("b", "/p/beta")]);
        st.select_root("/p/beta");
        // alpha vanishes; beta selection survives.
        st.fold_live_sessions(vec![ls("b", "/p/beta")]);
        assert_eq!(st.selected_root.as_deref(), Some("/p/beta"));
        // beta's session cd's elsewhere: the project list loses beta but the
        // selection stays (the directory is still real; panes churn cwds).
        let fetches = st.fold_live_sessions(vec![ls("z", "/p/zeta")]);
        assert_eq!(st.selected_root.as_deref(), Some("/p/beta"));
        assert!(fetches.is_empty());
        // A host-bound root that never appears as a project is sticky too.
        st.select_root("/somewhere/bound");
        st.fold_live_sessions(vec![ls("z", "/p/zeta")]);
        assert_eq!(st.selected_root.as_deref(), Some("/somewhere/bound"));
    }

    #[test]
    fn cycle_wraps_both_directions() {
        let mut st = PanelsState::new();
        st.fold_live_sessions(vec![ls("a", "/p/a"), ls("b", "/p/b"), ls("c", "/p/c")]);
        assert_eq!(st.selected_root.as_deref(), Some("/p/a"));
        st.cycle_project(-1);
        assert_eq!(st.selected_root.as_deref(), Some("/p/c"), "wraps backward");
        st.cycle_project(1);
        assert_eq!(st.selected_root.as_deref(), Some("/p/a"), "wraps forward");
    }
}
