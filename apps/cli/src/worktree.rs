//! Treehouse-style worktree lifecycle for `th worktree ls|prune|new` (task 25).
//!
//! The CLI interrogates git DIRECTLY for local facts (worktrees, dirtiness,
//! merge state) - it is a local tool and the repo is on this filesystem. The
//! control socket is only consulted for SESSION data: which live T-Hub session
//! is rooted in which worktree (a "lease"). Lease discovery is layered:
//!
//!   1. `list_terminals` over the control socket (the app fills `cwd` from
//!      tmux pane paths server-side).
//!   2. Any session the app reports without a cwd - or the whole list when the
//!      app is down - is correlated directly against the t-hub tmux socket
//!      (`tmux -L t-hub list-panes -a`), the same source of truth the app uses.
//!   3. If neither source is reachable, lease state is UNKNOWN and the caller
//!      must fail safe (prune refuses; ls shows `?`).
//!
//! Safety doctrine encoded here (the captain's never-reap-unlanded rules):
//!   - a DIRTY worktree is never removed, no flag overrides that;
//!   - a LEASED worktree (live session rooted at or under it) is hands-off;
//!   - an UNMERGED branch is only reaped with an explicit `--force`, and the
//!     plan prints exactly which commits would be lost.
//!
//! Note on lease granularity: a lease is detected from tmux `pane_current_path`,
//! so it tracks where each pane currently IS, not where it was spawned. A
//! session that `cd`-ed out of its worktree temporarily drops its lease; one
//! that `cd`-ed in acquires one. That bias is deliberate - it errs toward
//! whatever a live session is touching right now.

use std::collections::HashMap;
use std::process::Command;

use serde_json::{json, Value};

use crate::control;

// ---- git plumbing ------------------------------------------------------------

/// Run git with `-C dir`, capturing exit code + both streams. `Err` is reserved
/// for "could not run git at all"; a nonzero exit comes back as `Ok(code != 0)`.
fn run_git(dir: &str, args: &[&str]) -> Result<(Option<i32>, String, String), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Run git and require exit 0; returns trimmed stdout, or the stderr as error.
fn git_ok(dir: &str, args: &[&str]) -> Result<String, String> {
    let (code, stdout, stderr) = run_git(dir, args)?;
    if code == Some(0) {
        Ok(stdout.trim_end().to_string())
    } else {
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(format!("git {} failed: {}", args.join(" "), detail.trim()))
    }
}

// ---- `git worktree list --porcelain` parsing ---------------------------------

/// One entry from `git worktree list --porcelain`. The first entry is always
/// the main worktree.
#[derive(Debug, Clone, PartialEq)]
pub struct WtEntry {
    pub path: String,
    pub head: String,
    /// Short branch name (`refs/heads/` stripped); `None` when detached.
    pub branch: Option<String>,
    pub locked: bool,
    pub bare: bool,
    /// git itself flags the entry prunable (e.g. the directory was deleted
    /// out-of-band) - we skip these and point at `git worktree prune`.
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain` output. Entries are blank-line
/// separated blocks of `key[ value]` lines.
pub fn parse_worktree_porcelain(text: &str) -> Vec<WtEntry> {
    let mut out = Vec::new();
    let mut cur: Option<WtEntry> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            continue;
        }
        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, v),
            None => (line, ""),
        };
        match key {
            "worktree" => {
                if let Some(e) = cur.take() {
                    out.push(e);
                }
                cur = Some(WtEntry {
                    path: value.to_string(),
                    head: String::new(),
                    branch: None,
                    locked: false,
                    bare: false,
                    prunable: false,
                });
            }
            "HEAD" => {
                if let Some(e) = cur.as_mut() {
                    e.head = value.to_string();
                }
            }
            "branch" => {
                if let Some(e) = cur.as_mut() {
                    e.branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value).to_string());
                }
            }
            "locked" => {
                if let Some(e) = cur.as_mut() {
                    e.locked = true;
                }
            }
            "bare" => {
                if let Some(e) = cur.as_mut() {
                    e.bare = true;
                }
            }
            "prunable" => {
                if let Some(e) = cur.as_mut() {
                    e.prunable = true;
                }
            }
            // `detached` (bare key) and anything future: branch stays None / ignored.
            _ => {}
        }
    }
    if let Some(e) = cur.take() {
        out.push(e);
    }
    out
}

// ---- lease (session ↔ worktree) correlation ----------------------------------

/// One session's whereabouts: id + the pane's current path. `live` is false
/// only when the app reports a non-live state (a dead session prune may close).
#[derive(Debug, Clone)]
pub struct SessionPane {
    pub id: String,
    pub cwd: String,
    pub live: bool,
}

/// Where the lease data came from - surfaced verbatim in `--json` and notes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LeaseSource {
    /// The app answered `list_terminals` with cwds filled in.
    Control,
    /// The app answered but some cwds were filled from the tmux socket directly.
    ControlTmux,
    /// App down; sessions enumerated straight off the t-hub tmux socket.
    TmuxOnly,
    /// Neither source reachable - lease state is unknown (fail safe).
    Unavailable,
}

impl LeaseSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            LeaseSource::Control => "control",
            LeaseSource::ControlTmux => "control+tmux",
            LeaseSource::TmuxOnly => "tmux",
            LeaseSource::Unavailable => "unavailable",
        }
    }
}

/// The gathered session→cwd map plus provenance. `complete` is the safety bit:
/// true only when every LIVE session has a known cwd - prune requires it.
pub struct Leases {
    pub sessions: Vec<SessionPane>,
    pub source: LeaseSource,
    pub complete: bool,
    pub note: Option<String>,
    pub endpoint: Option<control::Endpoint>,
}

/// Is `cwd` at or under `root`? Plain path-prefix on `/`-separated components
/// (no canonicalization - both sides come from the same filesystem view).
fn path_within(cwd: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    let cwd = cwd.trim_end_matches('/');
    cwd == root || cwd.starts_with(&format!("{root}/"))
}

/// The worktree that OWNS `cwd`: the deepest worktree path containing it.
/// Linked worktrees nest under the repo root (`.claude/worktrees/...`), so a
/// plain prefix test would wrongly lease the main checkout to every crew
/// session - deepest-match attribution gives each session one home.
fn deepest_owner<'a>(cwd: &str, worktree_paths: &'a [String]) -> Option<&'a str> {
    worktree_paths
        .iter()
        .filter(|p| path_within(cwd, p))
        .max_by_key(|p| p.trim_end_matches('/').len())
        .map(String::as_str)
}

fn owned_by(s: &SessionPane, path: &str, worktree_paths: &[String]) -> bool {
    !s.cwd.is_empty()
        && deepest_owner(&s.cwd, worktree_paths)
            .is_some_and(|o| o.trim_end_matches('/') == path.trim_end_matches('/'))
}

/// The LIVE session leasing `path`, if any: attributed by deepest-worktree
/// ownership, smallest id shown when several sessions share the worktree.
pub fn lease_for(path: &str, worktree_paths: &[String], sessions: &[SessionPane]) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.live && owned_by(s, path, worktree_paths))
        .map(|s| s.id.clone())
        .min()
}

/// Sessions attributed to `path` that the app reports as NOT live - prune
/// closes these over the socket before removing the worktree.
pub fn dead_sessions_for(path: &str, worktree_paths: &[String], sessions: &[SessionPane]) -> Vec<String> {
    let mut ids: Vec<String> = sessions
        .iter()
        .filter(|s| !s.live && owned_by(s, path, worktree_paths))
        .map(|s| s.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Query the t-hub tmux socket directly: one `(session_name, pane_current_path)`
/// per pane. Mirrors the app's own `tmux::pane_info` source of truth. "No
/// server" on the socket means no sessions - that's an empty Vec, not an error.
fn tmux_panes() -> Result<Vec<(String, String)>, String> {
    let sock = std::env::var("T_HUB_TMUX_SOCKET").unwrap_or_else(|_| "t-hub".to_string());
    let out = tmux_command(&sock)
        .output()
        .map_err(|e| format!("failed to run tmux: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("no server running") || stderr.contains("error connecting to") {
            return Ok(Vec::new());
        }
        return Err(format!("tmux list-panes failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .filter_map(|l| {
            let (s, p) = l.split_once('\t')?;
            if s.is_empty() {
                return None;
            }
            Some((s.to_string(), p.to_string()))
        })
        .collect())
}

/// The tmux invocation is direct argv (no shell), so the `#{...}` format needs
/// no quoting armor here - that hazard was specific to the app's wsl.exe hop.
#[cfg(not(windows))]
fn tmux_command(sock: &str) -> Command {
    let mut c = Command::new("tmux");
    c.args(["-L", sock, "list-panes", "-a", "-F", "#{session_name}\t#{pane_current_path}"]);
    c
}

/// On a Windows-built `th` the tmux server lives in WSL; hop with `-e` so the
/// argv reaches tmux directly (never the user's default shell - see git.rs).
#[cfg(windows)]
fn tmux_command(sock: &str) -> Command {
    let mut c = Command::new("wsl.exe");
    c.args(["--cd", "~", "-e", "tmux", "-L", sock, "list-panes", "-a", "-F", "#{session_name}\t#{pane_current_path}"]);
    c
}

/// Gather session→cwd leases: control socket first, tmux to fill gaps or as the
/// app-down fallback. Never errors - degraded sources are reported in `source`
/// / `complete` / `note` so callers decide how safe to be.
pub fn gather_leases() -> Leases {
    let (endpoint, control_result) = match control::resolve_endpoint() {
        Ok(ep) => {
            let r = control::call(&ep, "list_terminals", json!({}));
            (Some(ep), Some(r))
        }
        Err(e) => (None, Some(Err(e))),
    };

    match control_result {
        Some(Ok(result)) => {
            let mut sessions: Vec<SessionPane> = result
                .get("terminals")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|t| {
                            let sfield = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let state = sfield("state");
                            SessionPane {
                                id: sfield("id"),
                                cwd: sfield("cwd"),
                                live: state.is_empty() || state == "live",
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Older app builds return cwd:"" - fill from the tmux socket, keyed
            // by the session's tmux name (`th_<id>`), the same key the app uses.
            let needs_fill = sessions.iter().any(|s| s.cwd.is_empty());
            if !needs_fill {
                return Leases { sessions, source: LeaseSource::Control, complete: true, note: None, endpoint };
            }
            match tmux_panes() {
                Ok(panes) => {
                    let by_session: HashMap<&str, &str> = panes
                        .iter()
                        .map(|(s, p)| (s.as_str(), p.as_str()))
                        .collect();
                    for s in sessions.iter_mut() {
                        if s.cwd.is_empty() {
                            let tmux_name = format!("th_{}", s.id);
                            if let Some(p) = by_session.get(tmux_name.as_str()).or_else(|| by_session.get(s.id.as_str())) {
                                s.cwd = p.to_string();
                            }
                        }
                    }
                    // Multi-pane sessions: a session leases every pane's path,
                    // so add the extra pane paths as additional rows.
                    let mut extra = Vec::new();
                    for (name, path) in &panes {
                        let id = name.strip_prefix("th_").unwrap_or(name);
                        if sessions.iter().any(|s| s.id == id && s.cwd == *path) {
                            continue;
                        }
                        if sessions.iter().any(|s| s.id == id) {
                            extra.push(SessionPane { id: id.to_string(), cwd: path.clone(), live: true });
                        }
                    }
                    sessions.extend(extra);
                    let complete = sessions.iter().all(|s| !s.live || !s.cwd.is_empty());
                    let note = (!complete)
                        .then(|| "some live sessions have no known cwd; they cannot be correlated to worktrees".to_string());
                    Leases { sessions, source: LeaseSource::ControlTmux, complete, note, endpoint }
                }
                Err(e) => {
                    let note = format!(
                        "the app reported sessions without cwds and the tmux fallback failed ({e}); lease state is incomplete"
                    );
                    Leases { sessions, source: LeaseSource::ControlTmux, complete: false, note: Some(note), endpoint }
                }
            }
        }
        _ => {
            // App down: the tmux socket alone is still authoritative for which
            // sessions exist (T-Hub terminals ARE tmux sessions on this socket).
            match tmux_panes() {
                Ok(panes) => {
                    let sessions = panes
                        .into_iter()
                        .filter(|(s, _)| s.starts_with("th_"))
                        .map(|(s, p)| SessionPane {
                            id: s.strip_prefix("th_").unwrap_or(&s).to_string(),
                            cwd: p,
                            live: true,
                        })
                        .collect();
                    Leases {
                        sessions,
                        source: LeaseSource::TmuxOnly,
                        complete: true,
                        note: Some("app down - leases read straight from the t-hub tmux socket".to_string()),
                        endpoint: None,
                    }
                }
                Err(e) => Leases {
                    sessions: Vec::new(),
                    source: LeaseSource::Unavailable,
                    complete: false,
                    note: Some(format!("neither the control socket nor tmux is reachable ({e})")),
                    endpoint: None,
                },
            }
        }
    }
}

// ---- classification -----------------------------------------------------------

/// Everything `ls`/`prune`/`new --reuse` need to know about one worktree.
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub path: String,
    pub head: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub locked: bool,
    /// git flagged the entry prunable (directory gone out-of-band).
    pub stale: bool,
    /// Uncommitted change count (`status --porcelain` lines); None = unknown.
    pub dirty: Option<u32>,
    /// Branch fully merged into the default branch; None when there is no
    /// branch (detached), the branch IS the default, or the check failed.
    pub merged: Option<bool>,
    /// Live session id rooted at or under this worktree, if any.
    pub lease: Option<String>,
}

/// A full repo snapshot: the answer to `th worktree ls`.
pub struct RepoScan {
    pub repo_root: String,
    pub default_branch: String,
    pub lease_source: LeaseSource,
    pub leases_complete: bool,
    pub lease_note: Option<String>,
    pub sessions: Vec<SessionPane>,
    pub endpoint: Option<control::Endpoint>,
    pub worktrees: Vec<WorktreeStatus>,
}

impl RepoScan {
    /// Every worktree path - the universe for deepest-owner lease attribution.
    pub fn worktree_paths(&self) -> Vec<String> {
        self.worktrees.iter().map(|w| w.path.clone()).collect()
    }
}

/// Resolve the MAIN worktree root for `dir` (default `.`): the first entry of
/// `git worktree list --porcelain` seen from anywhere inside the repo.
pub fn resolve_repo_root(dir: Option<&String>) -> Result<String, String> {
    let probe = dir.map(String::as_str).unwrap_or(".");
    let text = git_ok(probe, &["worktree", "list", "--porcelain"])?;
    parse_worktree_porcelain(&text)
        .first()
        .map(|e| e.path.clone())
        .ok_or_else(|| format!("no worktrees found for '{probe}' (is it a git repo?)"))
}

/// The repo's default branch: `origin/HEAD` if set, else `main`/`master` if
/// they exist locally, else whatever the main worktree has checked out.
fn default_branch(root: &str, main_branch: Option<&str>) -> String {
    if let Ok(sym) = git_ok(root, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some((_, b)) = sym.split_once('/') {
            if !b.is_empty() {
                return b.to_string();
            }
        }
    }
    for cand in ["main", "master"] {
        if let Ok((Some(0), _, _)) = run_git(root, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{cand}")]) {
            return cand.to_string();
        }
    }
    main_branch.unwrap_or("main").to_string()
}

/// Is `branch` fully merged into `default`? `merge-base --is-ancestor` exit 0
/// means yes, exit 1 means no, anything else is unknown.
fn branch_merged(root: &str, branch: &str, default: &str) -> Option<bool> {
    let b = format!("refs/heads/{branch}");
    let d = format!("refs/heads/{default}");
    match run_git(root, &["merge-base", "--is-ancestor", &b, &d]) {
        Ok((Some(0), _, _)) => Some(true),
        Ok((Some(1), _, _)) => Some(false),
        _ => None,
    }
}

/// Uncommitted change count for the worktree at `path` (tracked modifications
/// plus untracked files - the same cleanliness bar `git worktree remove` uses).
fn dirty_count(path: &str) -> Option<u32> {
    match git_ok(path, &["status", "--porcelain"]) {
        Ok(s) if s.is_empty() => Some(0),
        Ok(s) => Some(s.lines().count() as u32),
        Err(_) => None,
    }
}

/// The commits that deleting `rev` would lose (not reachable from `default`),
/// oldest last, capped for bounded output.
pub fn commits_lost(root: &str, rev: &str, default: &str) -> Vec<String> {
    let range = format!("refs/heads/{default}..{rev}");
    match git_ok(root, &["log", "--oneline", "--max-count=20", &range]) {
        Ok(s) if !s.is_empty() => s.lines().map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// Snapshot the repo: enumerate worktrees, classify each (dirty / merged /
/// leased), and carry the lease provenance for the caller's safety decisions.
pub fn scan(dir: Option<&String>) -> Result<RepoScan, String> {
    let repo_root = resolve_repo_root(dir)?;
    let text = git_ok(&repo_root, &["worktree", "list", "--porcelain"])?;
    let entries = parse_worktree_porcelain(&text);
    let main_branch = entries.first().and_then(|e| e.branch.clone());
    let default = default_branch(&repo_root, main_branch.as_deref());
    let leases = gather_leases();
    let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();

    let worktrees = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let is_main = i == 0;
            let merged = match &e.branch {
                Some(b) if *b == default => None,
                Some(b) => branch_merged(&repo_root, b, &default),
                None => None,
            };
            WorktreeStatus {
                path: e.path.clone(),
                head: e.head.clone(),
                branch: e.branch.clone(),
                is_main,
                locked: e.locked,
                stale: e.prunable,
                dirty: if e.prunable { None } else { dirty_count(&e.path) },
                merged,
                lease: lease_for(&e.path, &paths, &leases.sessions),
            }
        })
        .collect();

    Ok(RepoScan {
        repo_root,
        default_branch: default,
        lease_source: leases.source,
        leases_complete: leases.complete,
        lease_note: leases.note,
        sessions: leases.sessions,
        endpoint: leases.endpoint,
        worktrees,
    })
}

// ---- the prune decision table (pure - this is what the tests pin down) --------

/// What prune will do with one worktree, and why.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Safe to reap. `forced` marks an unmerged/unknown branch taken only
    /// because `--force` was passed (the plan prints what would be lost).
    Reap { forced: bool },
    Skip { reason: String },
}

/// The decision table. Ordering is the safety doctrine: main/locked/stale are
/// structural, then LEASED and DIRTY are absolute (no flag overrides them),
/// and only then does `force` get a say about unmerged branches.
pub fn prune_decision(w: &WorktreeStatus, default_branch: &str, force: bool) -> Decision {
    if w.is_main {
        return Decision::Skip { reason: "main worktree - never pruned".to_string() };
    }
    if w.locked {
        return Decision::Skip { reason: "locked (git worktree lock) - unlock it first".to_string() };
    }
    if w.stale {
        return Decision::Skip {
            reason: "stale entry (directory missing) - run `git worktree prune`".to_string(),
        };
    }
    if let Some(id) = &w.lease {
        return Decision::Skip { reason: format!("leased by live session {id} - hands off") };
    }
    match w.dirty {
        None => {
            return Decision::Skip { reason: "dirty state unknown (git status failed) - refusing".to_string() };
        }
        Some(n) if n > 0 => {
            return Decision::Skip {
                reason: format!("dirty ({n} uncommitted change{}) - never reaped", if n == 1 { "" } else { "s" }),
            };
        }
        Some(_) => {}
    }
    match &w.branch {
        None => {
            if force {
                Decision::Reap { forced: true }
            } else {
                Decision::Skip { reason: "detached HEAD (no branch) - --force to reap the worktree".to_string() }
            }
        }
        Some(b) if b == default_branch => {
            Decision::Skip { reason: format!("checked out on the default branch ({b})") }
        }
        Some(_) => match w.merged {
            Some(true) => Decision::Reap { forced: false },
            Some(false) if force => Decision::Reap { forced: true },
            Some(false) => Decision::Skip {
                reason: format!("branch not merged into {default_branch} - --force to reap (prints what would be lost)"),
            },
            None if force => Decision::Reap { forced: true },
            None => Decision::Skip {
                reason: format!("merge state vs {default_branch} unknown - --force to reap anyway"),
            },
        },
    }
}

// ---- reap execution ------------------------------------------------------------

/// The outcome of executing one reap (prune with `--yes`).
pub struct ReapResult {
    pub path: String,
    pub branch: Option<String>,
    pub closed_sessions: Vec<String>,
    pub removed: bool,
    pub branch_deleted: bool,
    pub error: Option<String>,
}

/// Reap one worktree: close its dead sessions over the socket (best-effort),
/// `git worktree remove` it, then delete the branch (`-d`, or `-D` when the
/// reap was forced past an unmerged branch). Dirty/leased were already ruled
/// out by [`prune_decision`]; `git worktree remove` re-checks cleanliness
/// itself as a final backstop, and we never pass it `--force`.
pub fn execute_reap(scan: &RepoScan, w: &WorktreeStatus, forced: bool) -> ReapResult {
    let mut res = ReapResult {
        path: w.path.clone(),
        branch: w.branch.clone(),
        closed_sessions: Vec::new(),
        removed: false,
        branch_deleted: false,
        error: None,
    };

    if let Some(ep) = &scan.endpoint {
        for id in dead_sessions_for(&w.path, &scan.worktree_paths(), &scan.sessions) {
            match control::call(ep, "close_terminal", json!({ "sessionId": id })) {
                Ok(_) => res.closed_sessions.push(id),
                Err(e) => {
                    res.error = Some(format!("failed to close dead session {id}: {e:?}"));
                    return res;
                }
            }
        }
    }

    if let Err(e) = git_ok(&scan.repo_root, &["worktree", "remove", &w.path]) {
        res.error = Some(e);
        return res;
    }
    res.removed = true;

    if let Some(b) = &w.branch {
        let flag = if forced { "-D" } else { "-d" };
        match git_ok(&scan.repo_root, &["branch", flag, b]) {
            Ok(_) => res.branch_deleted = true,
            Err(e) => res.error = Some(e),
        }
    }
    res
}

// ---- pool reuse for `worktree new` ---------------------------------------------

/// The derived pool path for a branch: `<repoRoot>/.claude/worktrees/<leaf>`
/// (this repo's own convention, matching `th worktree new`'s default).
pub fn pool_path(repo_root: &str, branch: &str) -> String {
    let leaf = branch.rsplit('/').next().unwrap_or(branch);
    format!("{}/.claude/worktrees/{}", repo_root.trim_end_matches('/'), leaf)
}

/// Is `path` inside the repo's worktree pool directory?
fn in_pool(repo_root: &str, path: &str) -> bool {
    path_within(path, &format!("{}/.claude/worktrees", repo_root.trim_end_matches('/')))
}

/// A worktree qualifies for reuse under exactly the reap conditions: linked,
/// unlocked, clean, unleased, and its branch fully merged. Same doctrine -
/// if it would be safe to prune, it is safe to recycle.
fn reusable(w: &WorktreeStatus, default_branch: &str) -> bool {
    matches!(prune_decision(w, default_branch, false), Decision::Reap { .. }) && w.branch.is_some()
}

/// What `worktree new` should do about the pool.
#[derive(Debug, PartialEq)]
pub enum ReusePlan {
    /// Recycle this worktree in place instead of growing the pool.
    Reuse(String),
    /// No safe candidate - fall through to the normal fresh create.
    Fresh,
    /// The target path is already a worktree that is NOT safe to recycle;
    /// creating fresh there would fail anyway, so surface the reason now.
    Conflict(String),
}

/// Pick a reuse candidate. With `--path` the choice is pinned to that exact
/// path; otherwise prefer the worktree already sitting at the derived pool
/// path, then the first (path-sorted) reusable pool worktree.
pub fn plan_reuse(
    scan: &RepoScan,
    explicit_path: Option<&String>,
    derived_path: &str,
) -> ReusePlan {
    let find_at = |p: &str| {
        let p = p.trim_end_matches('/');
        scan.worktrees.iter().find(|w| w.path.trim_end_matches('/') == p)
    };
    let conflict = |w: &WorktreeStatus| {
        let reason = match prune_decision(w, &scan.default_branch, false) {
            Decision::Skip { reason } => reason,
            Decision::Reap { .. } => "not recyclable".to_string(),
        };
        ReusePlan::Conflict(format!(
            "a worktree already exists at {} and is not safe to recycle: {reason}. \
             Pick a different --path, or reap it first with `th worktree prune`.",
            w.path
        ))
    };

    if let Some(p) = explicit_path {
        return match find_at(p) {
            Some(w) if reusable(w, &scan.default_branch) => ReusePlan::Reuse(w.path.clone()),
            Some(w) => conflict(w),
            None => ReusePlan::Fresh,
        };
    }

    if let Some(w) = find_at(derived_path) {
        return if reusable(w, &scan.default_branch) {
            ReusePlan::Reuse(w.path.clone())
        } else {
            conflict(w)
        };
    }

    let mut candidates: Vec<&WorktreeStatus> = scan
        .worktrees
        .iter()
        .filter(|w| in_pool(&scan.repo_root, &w.path) && reusable(w, &scan.default_branch))
        .collect();
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    match candidates.first() {
        Some(w) => ReusePlan::Reuse(w.path.clone()),
        None => ReusePlan::Fresh,
    }
}

/// The outcome of recycling a pool worktree onto a new branch.
pub struct ReuseOutcome {
    pub path: String,
    pub branch: String,
    pub previous_branch: Option<String>,
    pub base_commit: String,
    pub moved: bool,
    pub notes: Vec<String>,
}

/// Recycle `candidate` onto `new_branch` IN PLACE (preserving ignored build
/// artifacts - the whole point of a pool): optionally `git worktree move` it to
/// the canonical pool path, switch to the new branch (created at the repo
/// root's HEAD, mirroring `git worktree add`'s default base), then delete the
/// old, already-merged branch.
pub fn execute_reuse(
    scan: &RepoScan,
    candidate_path: &str,
    new_branch: &str,
    desired_path: &str,
) -> Result<ReuseOutcome, String> {
    let w = scan
        .worktrees
        .iter()
        .find(|w| w.path == candidate_path)
        .ok_or_else(|| format!("reuse candidate vanished: {candidate_path}"))?;
    let mut notes = Vec::new();

    // 1. Move the directory to the canonical pool path when it's free, so the
    //    slot's name follows the branch it now serves. Non-fatal on failure.
    let mut path = w.path.clone();
    if path.trim_end_matches('/') != desired_path.trim_end_matches('/') {
        if std::path::Path::new(desired_path).exists() {
            notes.push(format!("kept path {path} (target {desired_path} already exists)"));
        } else {
            match git_ok(&scan.repo_root, &["worktree", "move", &path, desired_path]) {
                Ok(_) => path = desired_path.to_string(),
                Err(e) => notes.push(format!("kept path {path} (worktree move failed: {e})")),
            }
        }
    }

    // 2. Switch to the new branch. Existing branch → adopt it as-is (matching
    //    the server's smart branch handling); otherwise create it at the repo
    //    root's current HEAD.
    let base = git_ok(&scan.repo_root, &["rev-parse", "HEAD"])?;
    let branch_ref = format!("refs/heads/{new_branch}");
    let exists = matches!(
        run_git(&scan.repo_root, &["show-ref", "--verify", "--quiet", &branch_ref]),
        Ok((Some(0), _, _))
    );
    if exists {
        git_ok(&path, &["switch", new_branch])?;
    } else {
        git_ok(&path, &["switch", "-c", new_branch, &base])?;
    }

    // 3. The old branch is fully merged (reuse requires it) - retire it.
    if let Some(old) = &w.branch {
        if old != new_branch {
            if let Err(e) = git_ok(&scan.repo_root, &["branch", "-d", old]) {
                notes.push(format!("old branch {old} not deleted: {e}"));
            }
        }
    }

    Ok(ReuseOutcome {
        path,
        branch: new_branch.to_string(),
        previous_branch: w.branch.clone(),
        base_commit: base,
        moved: w.path != desired_path && !notes.iter().any(|n| n.starts_with("kept path")),
        notes,
    })
}

// ---- JSON shapes (stable envelope payloads) --------------------------------------

/// One worktree as `--json` data, including the no-force prune verdict so
/// agents get the "why skipped" without a second call.
pub fn worktree_json(w: &WorktreeStatus, default_branch: &str) -> Value {
    let decision = prune_decision(w, default_branch, false);
    let (prunable, reason) = match &decision {
        Decision::Reap { .. } => (true, Value::Null),
        Decision::Skip { reason } => (false, json!(reason)),
    };
    json!({
        "path": w.path,
        "head": w.head,
        "branch": w.branch,
        "isMain": w.is_main,
        "locked": w.locked,
        "stale": w.stale,
        "dirty": w.dirty.map(|n| n > 0),
        "dirtyCount": w.dirty,
        "merged": w.merged,
        "leasedBy": w.lease,
        "prunable": prunable,
        "reason": reason,
    })
}

pub fn scan_json(scan: &RepoScan) -> Value {
    json!({
        "repoRoot": scan.repo_root,
        "defaultBranch": scan.default_branch,
        "leaseSource": scan.lease_source.as_str(),
        "leasesComplete": scan.leases_complete,
        "leaseNote": scan.lease_note,
        "count": scan.worktrees.len(),
        "worktrees": scan.worktrees.iter().map(|w| worktree_json(w, &scan.default_branch)).collect::<Vec<_>>(),
    })
}

// ---- tests: the classification + decision table, pure edges mocked ---------------

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, branch: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            path: path.to_string(),
            head: "abc1234".to_string(),
            branch: branch.map(str::to_string),
            is_main: false,
            locked: false,
            stale: false,
            dirty: Some(0),
            merged: Some(true),
            lease: None,
        }
    }

    fn skip_reason(d: Decision) -> String {
        match d {
            Decision::Skip { reason } => reason,
            Decision::Reap { forced } => panic!("expected Skip, got Reap {{ forced: {forced} }}"),
        }
    }

    // -- porcelain parsing --

    #[test]
    fn parses_porcelain_entries() {
        let text = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.claude/worktrees/feat
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feat

worktree /repo/.claude/worktrees/det
HEAD 3333333333333333333333333333333333333333
detached

worktree /repo/.claude/worktrees/locked-one
HEAD 4444444444444444444444444444444444444444
branch refs/heads/locked-branch
locked crew is using this

worktree /repo/.claude/worktrees/gone
HEAD 5555555555555555555555555555555555555555
branch refs/heads/gone
prunable gitdir file points to non-existent location
";
        let entries = parse_worktree_porcelain(text);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].path, "/repo");
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].branch.as_deref(), Some("feat"));
        assert_eq!(entries[2].branch, None);
        assert!(entries[3].locked);
        assert!(entries[4].prunable);
        assert!(!entries[1].locked && !entries[1].prunable);
    }

    #[test]
    fn parses_porcelain_without_trailing_blank_line() {
        let entries = parse_worktree_porcelain("worktree /r\nHEAD 9999\nbranch refs/heads/x");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("x"));
    }

    // -- lease correlation --

    fn pane(id: &str, cwd: &str, live: bool) -> SessionPane {
        SessionPane { id: id.to_string(), cwd: cwd.to_string(), live }
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lease_matches_exact_and_nested_paths() {
        let wts = paths(&["/repo", "/repo/.claude/worktrees/feat", "/repo/.claude/worktrees/other"]);
        let sessions = vec![
            pane("aaa", "/repo/.claude/worktrees/feat", true),
            pane("bbb", "/repo/.claude/worktrees/other/apps/native", true),
        ];
        assert_eq!(lease_for("/repo/.claude/worktrees/feat", &wts, &sessions), Some("aaa".into()));
        // A pane deep inside the worktree still leases it (th_t8demo-style).
        assert_eq!(lease_for("/repo/.claude/worktrees/other", &wts, &sessions), Some("bbb".into()));
        assert_eq!(lease_for("/repo/.claude/worktrees/none", &wts, &sessions), None);
    }

    #[test]
    fn nested_worktree_sessions_do_not_lease_the_main_checkout() {
        // Linked worktrees live UNDER the repo root: a crew session inside one
        // must lease only its own worktree, not the root; a session sitting at
        // the root itself does lease the root.
        let wts = paths(&["/repo", "/repo/.claude/worktrees/feat"]);
        let sessions = vec![pane("crew", "/repo/.claude/worktrees/feat", true)];
        assert_eq!(lease_for("/repo", &wts, &sessions), None);
        let sessions = vec![pane("capt", "/repo", true)];
        assert_eq!(lease_for("/repo", &wts, &sessions), Some("capt".into()));
    }

    #[test]
    fn lease_picks_smallest_session_id_when_several_share_a_worktree() {
        let wts = paths(&["/repo", "/repo/wt"]);
        let sessions = vec![pane("zzz", "/repo/wt", true), pane("aaa", "/repo/wt/sub", true)];
        assert_eq!(lease_for("/repo/wt", &wts, &sessions), Some("aaa".into()));
    }

    #[test]
    fn lease_does_not_match_sibling_prefix_or_dead_or_empty() {
        let wts = paths(&[
            "/repo",
            "/repo/.claude/worktrees/feat",
            "/repo/.claude/worktrees/feat-extra",
        ]);
        let sessions = vec![
            pane("aaa", "/repo/.claude/worktrees/feat-extra", true), // sibling, shares prefix
            pane("bbb", "/repo/.claude/worktrees/feat", false),      // dead session
            pane("ccc", "", true),                                   // unknown cwd
        ];
        assert_eq!(lease_for("/repo/.claude/worktrees/feat", &wts, &sessions), None);
        assert_eq!(
            dead_sessions_for("/repo/.claude/worktrees/feat", &wts, &sessions),
            vec!["bbb".to_string()]
        );
    }

    #[test]
    fn lease_ignores_trailing_slashes() {
        let wts = paths(&["/repo/wt"]);
        let sessions = vec![pane("aaa", "/repo/wt/", true)];
        assert_eq!(lease_for("/repo/wt", &wts, &sessions), Some("aaa".into()));
    }

    // -- the prune decision table --

    #[test]
    fn merged_clean_unleased_is_reaped() {
        assert_eq!(prune_decision(&wt("/r/w", Some("feat")), "main", false), Decision::Reap { forced: false });
    }

    #[test]
    fn main_worktree_is_never_pruned() {
        let mut w = wt("/r", Some("main"));
        w.is_main = true;
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("main worktree"));
    }

    #[test]
    fn locked_is_skipped() {
        let mut w = wt("/r/w", Some("feat"));
        w.locked = true;
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("locked"));
    }

    #[test]
    fn stale_is_skipped_and_points_at_git_worktree_prune() {
        let mut w = wt("/r/w", Some("feat"));
        w.stale = true;
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("git worktree prune"));
    }

    #[test]
    fn leased_is_hands_off_even_with_force() {
        let mut w = wt("/r/w", Some("feat"));
        w.lease = Some("052ccbb2".to_string());
        let reason = skip_reason(prune_decision(&w, "main", true));
        assert!(reason.contains("leased"), "reason: {reason}");
        assert!(reason.contains("052ccbb2"));
    }

    #[test]
    fn dirty_is_never_reaped_even_with_force() {
        let mut w = wt("/r/w", Some("feat"));
        w.dirty = Some(3);
        let reason = skip_reason(prune_decision(&w, "main", true));
        assert!(reason.contains("dirty (3 uncommitted changes)"), "reason: {reason}");
    }

    #[test]
    fn unknown_dirty_state_is_skipped() {
        let mut w = wt("/r/w", Some("feat"));
        w.dirty = None;
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("dirty state unknown"));
    }

    #[test]
    fn unmerged_needs_force() {
        let mut w = wt("/r/w", Some("feat"));
        w.merged = Some(false);
        assert!(skip_reason(prune_decision(&w, "main", false)).contains("not merged"));
        assert_eq!(prune_decision(&w, "main", true), Decision::Reap { forced: true });
    }

    #[test]
    fn unknown_merge_state_needs_force() {
        let mut w = wt("/r/w", Some("feat"));
        w.merged = None;
        assert!(skip_reason(prune_decision(&w, "main", false)).contains("unknown"));
        assert_eq!(prune_decision(&w, "main", true), Decision::Reap { forced: true });
    }

    #[test]
    fn detached_head_needs_force() {
        let mut w = wt("/r/w", None);
        w.merged = None;
        assert!(skip_reason(prune_decision(&w, "main", false)).contains("detached"));
        assert_eq!(prune_decision(&w, "main", true), Decision::Reap { forced: true });
    }

    #[test]
    fn default_branch_checkout_is_skipped() {
        let w = wt("/r/w", Some("main"));
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("default branch"));
    }

    #[test]
    fn lease_outranks_dirty_outranks_unmerged() {
        // The reason reported is the strongest protection, in doctrine order.
        let mut w = wt("/r/w", Some("feat"));
        w.merged = Some(false);
        w.dirty = Some(1);
        w.lease = Some("aaa".to_string());
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("leased"));
        w.lease = None;
        assert!(skip_reason(prune_decision(&w, "main", true)).contains("dirty"));
    }

    // -- reuse planning --

    fn scan_with(worktrees: Vec<WorktreeStatus>) -> RepoScan {
        RepoScan {
            repo_root: "/repo".to_string(),
            default_branch: "main".to_string(),
            lease_source: LeaseSource::Control,
            leases_complete: true,
            lease_note: None,
            sessions: Vec::new(),
            endpoint: None,
            worktrees,
        }
    }

    #[test]
    fn reuse_picks_reusable_pool_worktree() {
        let mut main = wt("/repo", Some("main"));
        main.is_main = true;
        let scan = scan_with(vec![main, wt("/repo/.claude/worktrees/done", Some("done"))]);
        assert_eq!(
            plan_reuse(&scan, None, "/repo/.claude/worktrees/new-task"),
            ReusePlan::Reuse("/repo/.claude/worktrees/done".to_string())
        );
    }

    #[test]
    fn reuse_prefers_candidate_already_at_derived_path_and_sorts_rest() {
        let a = wt("/repo/.claude/worktrees/aaa", Some("aaa"));
        let b = wt("/repo/.claude/worktrees/new-task", Some("old"));
        let scan = scan_with(vec![b.clone(), a.clone()]);
        assert_eq!(
            plan_reuse(&scan, None, "/repo/.claude/worktrees/new-task"),
            ReusePlan::Reuse("/repo/.claude/worktrees/new-task".to_string())
        );
        let scan = scan_with(vec![wt("/repo/.claude/worktrees/zzz", Some("z")), a]);
        assert_eq!(
            plan_reuse(&scan, None, "/repo/.claude/worktrees/new-task"),
            ReusePlan::Reuse("/repo/.claude/worktrees/aaa".to_string())
        );
    }

    #[test]
    fn reuse_never_touches_dirty_leased_or_unmerged_candidates() {
        let mut dirty = wt("/repo/.claude/worktrees/a", Some("a"));
        dirty.dirty = Some(2);
        let mut leased = wt("/repo/.claude/worktrees/b", Some("b"));
        leased.lease = Some("052ccbb2".to_string());
        let mut unmerged = wt("/repo/.claude/worktrees/c", Some("c"));
        unmerged.merged = Some(false);
        let scan = scan_with(vec![dirty, leased, unmerged]);
        assert_eq!(plan_reuse(&scan, None, "/repo/.claude/worktrees/new"), ReusePlan::Fresh);
    }

    #[test]
    fn reuse_ignores_clean_worktrees_outside_the_pool() {
        let scan = scan_with(vec![wt("/somewhere/else", Some("done"))]);
        assert_eq!(plan_reuse(&scan, None, "/repo/.claude/worktrees/new"), ReusePlan::Fresh);
    }

    #[test]
    fn derived_path_occupied_by_unsafe_worktree_is_a_conflict() {
        let mut w = wt("/repo/.claude/worktrees/new-task", Some("old"));
        w.lease = Some("052ccbb2".to_string());
        let scan = scan_with(vec![w]);
        match plan_reuse(&scan, None, "/repo/.claude/worktrees/new-task") {
            ReusePlan::Conflict(msg) => assert!(msg.contains("leased"), "msg: {msg}"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn explicit_path_pins_the_choice() {
        let a = wt("/repo/.claude/worktrees/aaa", Some("aaa"));
        let scan = scan_with(vec![a]);
        let p = "/repo/.claude/worktrees/aaa".to_string();
        assert_eq!(plan_reuse(&scan, Some(&p), "/repo/.claude/worktrees/derived"), ReusePlan::Reuse(p.clone()));
        let missing = "/repo/elsewhere".to_string();
        assert_eq!(plan_reuse(&scan, Some(&missing), "/repo/.claude/worktrees/derived"), ReusePlan::Fresh);
    }

    #[test]
    fn explicit_path_at_unsafe_worktree_is_a_conflict() {
        let mut w = wt("/repo/.claude/worktrees/aaa", Some("aaa"));
        w.dirty = Some(1);
        let scan = scan_with(vec![w]);
        let p = "/repo/.claude/worktrees/aaa".to_string();
        match plan_reuse(&scan, Some(&p), "/repo/.claude/worktrees/derived") {
            ReusePlan::Conflict(msg) => assert!(msg.contains("dirty"), "msg: {msg}"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn detached_pool_worktree_is_not_reused() {
        // Clean + unleased but detached: prune (with force) may reap it, but
        // reuse requires a branch to retire.
        let mut w = wt("/repo/.claude/worktrees/det", None);
        w.merged = None;
        let scan = scan_with(vec![w]);
        assert_eq!(plan_reuse(&scan, None, "/repo/.claude/worktrees/new"), ReusePlan::Fresh);
    }

    // -- pool path derivation --

    #[test]
    fn pool_path_uses_branch_leaf_and_trims_root_slash() {
        assert_eq!(pool_path("/repo/", "crew/feat-x"), "/repo/.claude/worktrees/feat-x");
        assert_eq!(pool_path("/repo", "feat-y"), "/repo/.claude/worktrees/feat-y");
    }
}
