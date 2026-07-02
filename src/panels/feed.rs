//! The panels data pump (T11): one background poll/action thread that keeps
//! [`PanelsState`] fresh off the §1.3 ControlClient, mirroring the T9
//! `OverlayFeed` shape.
//!
//! Deliberately NO `ControlClient::events()` subscription: the wire's event
//! channel is competing-consumer and the T9 OverlayFeed must remain the
//! process's single drainer (execution doc §5 merge caution). Panels are pure
//! command-poll consumers, so they compose beside the overlays in any host.
//!
//! Cadences live in the pure [`PanelPlan`] (unit-tested); UI actions arrive on
//! a channel and are serviced between ticks; URL probes run on short-lived
//! threads so a slow connect never stalls the poll loop.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use serde_json::json;

use super::files::{DirEntry, FetchDir, FileContents, GitInfo, IndexSummary, SearchResponse, SEARCH_LIMIT};
use super::preview::scan_local_urls;
use super::probe::probe_url;
use super::runner::RunnerCmd;
use super::{now_ms, LiveSession, PanelsState};
use crate::wire::ControlClient;

/// `list_terminals` cadence (matches the T9 live-sessions poll).
pub const SESSIONS_POLL_MS: u64 = 10_000;
/// Faster session cadence while a runner spawn awaits identification.
pub const SESSIONS_SPAWNING_POLL_MS: u64 = 1_000;
/// Capture-scan cadence for the preview URL sweep.
pub const PREVIEW_SCAN_MS: u64 = 10_000;
/// Capture-poll cadence for active runner tails.
pub const RUNNER_TAIL_MS: u64 = 1_000;
/// `git_info` cadence for the selected root (webview polls per-tile at 5s;
/// one selected root at 30s is plenty for a panel header).
pub const GIT_POLL_MS: u64 = 30_000;
/// Poll-thread tick (also the action-servicing + search-debounce latency bound).
const TICK_MS: u64 = 100;
/// `read_terminal` historyLines for the preview URL sweep.
pub const PREVIEW_HISTORY_LINES: u64 = 200;
/// `read_terminal` historyLines for runner tails.
pub const RUNNER_HISTORY_LINES: u64 = 300;

/// UI-initiated actions, sent by the render layer (or the embedding host).
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAction {
    /// Point Files + Run at this project root (host hook for per-tile panels).
    SelectRoot(String),
    /// Cycle the project picker by +-1.
    CycleProject(isize),
    ToggleDir(String),
    SetQuery(String),
    OpenFile(String),
    CloseViewer,
    ToggleDotfiles,
    ToggleShowIgnored,
    RefreshTree,
    /// Host push: URLs scanned from an attached tile's grid
    /// (`TermSession::visible_urls()` text) for `session`.
    NoteSessionUrls { session: String, urls: Vec<String> },
    /// Re-check one preview URL.
    Reprobe { session: String, canonical: String },
    RunnerStart,
    RunnerStop,
    RunnerKill,
    RunnerSetCommand(String),
}

/// Which poll is due (see [`PanelPlan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFetch {
    Sessions,
    PreviewScan,
    RunnerTails,
    Git,
}

/// Pure cadence scheduler for the poll thread. Deterministic (caller passes
/// `now_ms` + the gating flags), so it unit-tests without threads or sockets.
#[derive(Debug, Default)]
pub struct PanelPlan {
    next_sessions_ms: u64,
    next_preview_ms: u64,
    next_tail_ms: u64,
    next_git_ms: u64,
}

impl PanelPlan {
    pub fn new() -> Self {
        Self::default()
    }

    /// The fetches due at `now_ms`, arming each one's next deadline.
    /// `spawning` tightens the sessions cadence (a runner is waiting to
    /// identify its spawned session); `has_tails` / `root_selected` gate the
    /// polls that would otherwise be no-ops.
    pub fn due(
        &mut self,
        now_ms: u64,
        spawning: bool,
        has_tails: bool,
        root_selected: bool,
    ) -> Vec<PanelFetch> {
        let mut out = Vec::new();
        if now_ms >= self.next_sessions_ms {
            let cadence = if spawning { SESSIONS_SPAWNING_POLL_MS } else { SESSIONS_POLL_MS };
            self.next_sessions_ms = now_ms + cadence;
            out.push(PanelFetch::Sessions);
        } else if spawning {
            // A spawn just started: pull the next sweep forward if the normal
            // cadence would leave it waiting seconds away.
            self.next_sessions_ms = self.next_sessions_ms.min(now_ms + SESSIONS_SPAWNING_POLL_MS);
        }
        if now_ms >= self.next_preview_ms {
            self.next_preview_ms = now_ms + PREVIEW_SCAN_MS;
            out.push(PanelFetch::PreviewScan);
        }
        if has_tails && now_ms >= self.next_tail_ms {
            self.next_tail_ms = now_ms + RUNNER_TAIL_MS;
            out.push(PanelFetch::RunnerTails);
        }
        if root_selected && now_ms >= self.next_git_ms {
            self.next_git_ms = now_ms + GIT_POLL_MS;
            out.push(PanelFetch::Git);
        }
        out
    }

    /// Make the next tick re-list sessions (after a spawn/kill).
    pub fn force_sessions(&mut self) {
        self.next_sessions_ms = 0;
    }

    /// Make the next tick refetch git info (after a root change).
    pub fn force_git(&mut self) {
        self.next_git_ms = 0;
    }
}

/// The running feed: shared state + the action channel. Cheap to clone; all
/// clones share the same underlying feed.
#[derive(Clone)]
pub struct PanelsFeed {
    state: Arc<Mutex<PanelsState>>,
    actions_tx: Sender<PanelAction>,
}

impl PanelsFeed {
    /// Start the feed on `client`: spawns the poll/action thread and returns
    /// the shared handle. The thread runs for the process lifetime.
    pub fn spawn(client: Arc<ControlClient>) -> Self {
        let state = Arc::new(Mutex::new(PanelsState::new()));
        let (actions_tx, actions_rx) = unbounded::<PanelAction>();
        {
            let state = state.clone();
            thread::spawn(move || poll_loop(client, state, actions_rx));
        }
        PanelsFeed { state, actions_tx }
    }

    /// The shared panels state (lock, read/reduce, drop the guard before paint).
    pub fn state(&self) -> Arc<Mutex<PanelsState>> {
        self.state.clone()
    }

    /// Send a UI action (fire-and-forget).
    pub fn send(&self, action: PanelAction) {
        let _ = self.actions_tx.send(action);
    }

    /// Host hook: push URLs detected on an attached tile's own grid (T6
    /// `visible_urls`). `session` may be either id space (`th_*` or bare id).
    pub fn note_session_urls(&self, session: &str, urls: Vec<String>) {
        self.send(PanelAction::NoteSessionUrls { session: session.to_string(), urls });
    }

    /// Host hook: bind the Files + Run tabs to a root (per-tile panel usage).
    pub fn set_root(&self, root: &str) {
        self.send(PanelAction::SelectRoot(root.to_string()));
    }
}

fn poll_loop(
    client: Arc<ControlClient>,
    state: Arc<Mutex<PanelsState>>,
    actions_rx: Receiver<PanelAction>,
) {
    let mut plan = PanelPlan::new();
    loop {
        let now = now_ms();
        let (tick_cmds, spawning, has_tails, root_selected) = {
            let mut st = state.lock();
            let tick_cmds = st.runners.on_tick(now);
            (
                tick_cmds,
                st.runners.any_spawning(),
                !st.runners.tail_targets().is_empty(),
                st.selected_root.is_some(),
            )
        };
        exec_runner_cmds(&client, &state, None, tick_cmds);
        for fetch in plan.due(now, spawning, has_tails, root_selected) {
            run_fetch(fetch, &client, &state);
        }

        // Default-command detection for freshly created runners.
        let defaults: Vec<String> = state.lock().runners.take_default_fetches();
        for root in defaults {
            let result = fetch_dir_entries(&client, &root, false);
            let mut st = state.lock();
            if let (Some(r), Ok(entries)) = (st.runners.get_mut(&root), &result) {
                r.fold_default_dir(entries);
            }
        }

        // Debounced fuzzy search (index on first use, webview parity).
        let due = {
            let mut st = state.lock();
            let root = st.files.root.clone();
            st.files
                .take_due_search(now_ms())
                .and_then(|(seq, q)| root.map(|r| (seq, q, r, st.files.indexed)))
        };
        if let Some((seq, query, root, indexed)) = due {
            run_search(&client, &state, seq, &query, &root, indexed);
        }

        // Kick off probes for newly seen URLs.
        let unprobed = state.lock().preview.take_unprobed();
        for (session, url) in unprobed {
            let state = state.clone();
            thread::spawn(move || {
                let outcome = probe_url(&url);
                state.lock().preview.fold_probe(
                    &session,
                    &url.canonical(),
                    outcome.probe,
                    outcome.title,
                );
            });
        }

        match actions_rx.recv_timeout(Duration::from_millis(TICK_MS)) {
            Ok(action) => run_action(action, &client, &state, &mut plan),
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam::channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn run_fetch(fetch: PanelFetch, client: &ControlClient, state: &Arc<Mutex<PanelsState>>) {
    match fetch {
        PanelFetch::Sessions => {
            let live = match client.request("list_terminals", json!({})) {
                Ok(v) => v["terminals"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .map(|t| LiveSession {
                                id: t["id"].as_str().unwrap_or("").to_string(),
                                title: t["title"].as_str().unwrap_or("").to_string(),
                                cwd: t["cwd"].as_str().unwrap_or("").to_string(),
                            })
                            .filter(|s| !s.id.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                Err(e) => {
                    state.lock().error = Some(e.to_string());
                    return;
                }
            };
            let now = now_ms();
            let (fetches, cmds) = {
                let mut st = state.lock();
                let fetches = st.fold_live_sessions(live);
                // The selected project always has a runner, so its default
                // command resolves (pnpm-lock detection) before first use.
                if let Some(root) = st.selected_root.clone() {
                    st.runners.ensure(&root, now);
                }
                let sessions = st.live.clone();
                let cmds = st.runners.on_sessions(&sessions, now);
                (fetches, cmds)
            };
            for f in fetches {
                run_dir_fetch(client, state, &f);
            }
            exec_runner_cmds(client, state, None, cmds);
        }
        PanelFetch::PreviewScan => {
            let targets = state.lock().preview.scan_targets();
            for sid in targets {
                let Ok(v) = client.request(
                    "read_terminal",
                    json!({ "sessionId": sid, "historyLines": PREVIEW_HISTORY_LINES }),
                ) else {
                    continue;
                };
                let text = v["text"].as_str().unwrap_or("");
                let urls = scan_local_urls(text);
                if !urls.is_empty() {
                    state.lock().preview.fold_urls(&sid, urls, now_ms());
                }
            }
        }
        PanelFetch::RunnerTails => {
            let targets = state.lock().runners.tail_targets();
            for (root, sid) in targets {
                let Ok(v) = client.request(
                    "read_terminal",
                    json!({ "sessionId": sid, "historyLines": RUNNER_HISTORY_LINES }),
                ) else {
                    continue;
                };
                let text = v["text"].as_str().unwrap_or("").to_string();
                let mut st = state.lock();
                if let Some(r) = st.runners.get_mut(&root) {
                    r.on_capture(&sid, &text, now_ms());
                }
            }
        }
        PanelFetch::Git => {
            let Some(root) = state.lock().selected_root.clone() else { return };
            if let Ok(v) = client.request("git_info", json!({ "path": root })) {
                if let Ok(info) = serde_json::from_value::<GitInfo>(v) {
                    let mut st = state.lock();
                    if st.selected_root.as_deref() == Some(root.as_str()) {
                        st.files.fold_git(info);
                    }
                }
            }
        }
    }
}

/// `list_dir` + decode (shared by tree fetches and default-command detection).
fn fetch_dir_entries(
    client: &ControlClient,
    path: &str,
    show_ignored: bool,
) -> Result<Vec<DirEntry>, String> {
    client
        .request("list_dir", json!({ "path": path, "showIgnored": show_ignored }))
        .map_err(|e| e.to_string())
        .and_then(|v| serde_json::from_value::<Vec<DirEntry>>(v).map_err(|e| e.to_string()))
}

fn run_dir_fetch(client: &ControlClient, state: &Arc<Mutex<PanelsState>>, fetch: &FetchDir) {
    let result = fetch_dir_entries(client, &fetch.path, fetch.show_ignored);
    state.lock().files.fold_dir(&fetch.path, result);
}

fn run_search(
    client: &ControlClient,
    state: &Arc<Mutex<PanelsState>>,
    seq: u64,
    query: &str,
    root: &str,
    indexed: bool,
) {
    if !indexed {
        match client.request("index_project", json!({ "root": root })) {
            Ok(v) => {
                if let Ok(sum) = serde_json::from_value::<IndexSummary>(v) {
                    state.lock().files.fold_index(sum);
                }
            }
            Err(e) => {
                // search_files self-indexes on demand server-side, so a failed
                // explicit index only surfaces as a hint, not a dead search.
                log::warn!("panels: index_project failed: {e}");
            }
        }
    }
    let result = client
        .request(
            "search_files",
            json!({ "root": root, "query": query, "limit": SEARCH_LIMIT }),
        )
        .map_err(|e| e.to_string())
        .and_then(|v| serde_json::from_value::<SearchResponse>(v).map_err(|e| e.to_string()));
    state.lock().files.fold_hits(seq, result);
}

/// Execute runner-emitted socket commands. `spawn_root` names the runner to
/// fail fast when a `Spawn` is rejected (e.g. headless server, no UI sink to
/// adopt the tile); other command failures surface via the machine's own
/// timeouts/session-sweep, so they are only logged.
pub fn exec_runner_cmds(
    client: &ControlClient,
    state: &Arc<Mutex<PanelsState>>,
    spawn_root: Option<&str>,
    cmds: Vec<RunnerCmd>,
) {
    for cmd in cmds {
        let result = match &cmd {
            RunnerCmd::Spawn { cwd, name } => {
                client.request("spawn_terminal", json!({ "cwd": cwd, "name": name }))
            }
            RunnerCmd::SendText { sid, text } => client.request(
                "send_text",
                json!({ "sessionId": sid, "text": text, "enter": true }),
            ),
            RunnerCmd::SendKeys { sid, keys } => {
                client.request("send_keys", json!({ "sessionId": sid, "keys": keys }))
            }
            RunnerCmd::Kill { sid } => {
                client.request("close_terminal", json!({ "sessionId": sid }))
            }
        };
        if let Err(e) = result {
            log::warn!("panels: runner command {cmd:?} failed: {e}");
            if let (RunnerCmd::Spawn { .. }, Some(root)) = (&cmd, spawn_root) {
                let mut st = state.lock();
                if let Some(r) = st.runners.get_mut(root) {
                    r.fail(format!("spawn refused: {e}"));
                }
            }
        }
    }
}

fn run_action(
    action: PanelAction,
    client: &ControlClient,
    state: &Arc<Mutex<PanelsState>>,
    plan: &mut PanelPlan,
) {
    match action {
        PanelAction::SelectRoot(root) => {
            let fetches = {
                let mut st = state.lock();
                let fetches = st.select_root(&root);
                st.runners.ensure(&root, now_ms());
                fetches
            };
            plan.force_git();
            for f in fetches {
                run_dir_fetch(client, state, &f);
            }
        }
        PanelAction::CycleProject(delta) => {
            let fetches = {
                let mut st = state.lock();
                let fetches = st.cycle_project(delta);
                if let Some(root) = st.selected_root.clone() {
                    st.runners.ensure(&root, now_ms());
                }
                fetches
            };
            plan.force_git();
            for f in fetches {
                run_dir_fetch(client, state, &f);
            }
        }
        PanelAction::ToggleDir(path) => {
            let fetch = state.lock().files.toggle_dir(&path);
            if let Some(f) = fetch {
                run_dir_fetch(client, state, &f);
            }
        }
        PanelAction::SetQuery(q) => state.lock().files.set_query(&q, now_ms()),
        PanelAction::OpenFile(path) => {
            let fetch = state.lock().files.open(&path);
            if let Some(path) = fetch {
                let result = client
                    .request("open_file", json!({ "path": path }))
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        serde_json::from_value::<FileContents>(v).map_err(|e| e.to_string())
                    });
                state.lock().files.fold_file(&path, result);
            }
        }
        PanelAction::CloseViewer => state.lock().files.close_viewer(),
        PanelAction::ToggleDotfiles => {
            let mut st = state.lock();
            st.files.hide_dotfiles = !st.files.hide_dotfiles;
        }
        PanelAction::ToggleShowIgnored => {
            let fetches = {
                let mut st = state.lock();
                let show = !st.files.show_ignored;
                st.files.set_show_ignored(show)
            };
            for f in fetches {
                run_dir_fetch(client, state, &f);
            }
        }
        PanelAction::RefreshTree => {
            let fetches = state.lock().files.refresh();
            for f in fetches {
                run_dir_fetch(client, state, &f);
            }
        }
        PanelAction::NoteSessionUrls { session, urls } => {
            let parsed: Vec<_> =
                urls.iter().filter_map(|u| super::preview::parse_local_url(u)).collect();
            if !parsed.is_empty() {
                state.lock().preview.fold_urls(&session, parsed, now_ms());
            }
        }
        PanelAction::Reprobe { session, canonical } => {
            state.lock().preview.reprobe(&session, &canonical);
        }
        PanelAction::RunnerStart => {
            let now = now_ms();
            let (root, cmds) = {
                let mut st = state.lock();
                let Some(root) = st.selected_root.clone() else { return };
                let live = st.live.clone();
                let r = st.runners.ensure(&root, now);
                (root, r.start(now, &live))
            };
            plan.force_sessions();
            exec_runner_cmds(client, state, Some(&root), cmds);
        }
        PanelAction::RunnerStop => {
            let cmds = {
                let mut st = state.lock();
                let Some(root) = st.selected_root.clone() else { return };
                st.runners.get_mut(&root).map(|r| r.stop(now_ms())).unwrap_or_default()
            };
            exec_runner_cmds(client, state, None, cmds);
        }
        PanelAction::RunnerKill => {
            let cmds = {
                let mut st = state.lock();
                let Some(root) = st.selected_root.clone() else { return };
                st.runners.get_mut(&root).map(|r| r.kill()).unwrap_or_default()
            };
            plan.force_sessions();
            exec_runner_cmds(client, state, None, cmds);
        }
        PanelAction::RunnerSetCommand(cmd) => {
            let mut st = state.lock();
            let Some(root) = st.selected_root.clone() else { return };
            let now = now_ms();
            st.runners.ensure(&root, now).set_command(&cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_plan_fetches_sessions_and_preview_then_respects_cadence() {
        let mut plan = PanelPlan::new();
        let first = plan.due(0, false, false, false);
        assert_eq!(first, vec![PanelFetch::Sessions, PanelFetch::PreviewScan]);
        assert!(plan.due(1, false, false, false).is_empty());
        // Sessions comes due at its cadence.
        assert!(plan
            .due(SESSIONS_POLL_MS, false, false, false)
            .contains(&PanelFetch::Sessions));
    }

    #[test]
    fn tails_and_git_are_gated_by_their_flags() {
        let mut plan = PanelPlan::new();
        assert!(!plan.due(0, false, false, false).contains(&PanelFetch::RunnerTails));
        assert!(plan.due(1, false, true, false).contains(&PanelFetch::RunnerTails));
        // Tail cadence is 1s.
        assert!(!plan.due(500, false, true, false).contains(&PanelFetch::RunnerTails));
        assert!(plan
            .due(1 + RUNNER_TAIL_MS, false, true, false)
            .contains(&PanelFetch::RunnerTails));
        // Git only with a selected root, then on its slow cadence.
        assert!(plan.due(2000, false, false, true).contains(&PanelFetch::Git));
        assert!(!plan.due(2001, false, false, true).contains(&PanelFetch::Git));
    }

    #[test]
    fn spawning_tightens_the_sessions_cadence() {
        let mut plan = PanelPlan::new();
        plan.due(0, false, false, false); // arms next at +10s
        // Not due yet normally...
        assert!(plan.due(2_000, false, false, false).is_empty());
        // ...but a spawn pulls the next sweep to within 1s.
        plan.due(2_100, true, false, false);
        let soon = plan.due(2_100 + SESSIONS_SPAWNING_POLL_MS, true, false, false);
        assert!(soon.contains(&PanelFetch::Sessions));
        // And while spawning, sessions re-arm at the fast cadence.
        let next = plan.due(2_100 + 2 * SESSIONS_SPAWNING_POLL_MS + 1, true, false, false);
        assert!(next.contains(&PanelFetch::Sessions));
    }

    #[test]
    fn force_sessions_and_git_pull_fetches_forward() {
        let mut plan = PanelPlan::new();
        plan.due(0, false, false, true);
        assert!(plan.due(100, false, false, true).is_empty());
        plan.force_sessions();
        plan.force_git();
        let due = plan.due(101, false, false, true);
        assert!(due.contains(&PanelFetch::Sessions));
        assert!(due.contains(&PanelFetch::Git));
    }
}
