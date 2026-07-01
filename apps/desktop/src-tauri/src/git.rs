//! Git awareness for the Files panel (feat/git-panel): report the current branch
//! + worktree for a project cwd, and commit changes from the panel.
//!
//! Scope (deliberately small + best-effort):
//!   - `git_info(cwd)`  — branch, worktree root, linked-worktree flag, dirty count.
//!     A non-repo (or an unreadable dir) reports `is_repo: false` rather than
//!     erroring, so the UI can simply render nothing.
//!   - `git_commit(cwd, message)` — `git add -A` then `git commit -m <message>`,
//!     returning the new short hash (or git's output). Empty messages are rejected.
//!
//! Path handling mirrors `files.rs`: on unix a native POSIX path already *is* the
//! Linux path, so we run `git` directly with `.current_dir(cwd)`. On Windows the
//! desktop app shells into WSL, so we invoke `git` *inside* the distro via
//! `wsl.exe`. IMPORTANT quoting caveat we hit: combining `wsl.exe` with a complex
//! `bash -lc '<script>'` can MANGLE a trailing argument. We sidestep that entirely
//! by using wsl.exe's own `--cd <posix-cwd>` flag and invoking git directly with
//! NO bash script — git is on the default PATH (/usr/bin), so no login shell is
//! needed. The commit message is always passed as a separate argv arg to git
//! (never interpolated into a shell string), so messages with shell metacharacters
//! are safe.
//!
//! Boundaries: this module is self-contained. It owns no managed state and shares
//! nothing with the file index / agent / supervision modules.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Git facts about a project cwd, surfaced to the Files panel header. Serialized
/// camelCase to mirror the TS `GitInfo` interface in `src/ipc/git.ts`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    /// True when `cwd` is inside a git working tree.
    pub is_repo: bool,
    /// The current branch (e.g. `main`), or `None` on a detached HEAD / non-repo.
    pub branch: Option<String>,
    /// Absolute path to this working tree's root (`rev-parse --show-toplevel`).
    pub worktree_root: Option<String>,
    /// True when `cwd` is a *linked* worktree (created via `git worktree add`),
    /// i.e. its per-worktree git dir differs from the repo's common git dir.
    pub is_linked_worktree: bool,
    /// Number of changed entries (`git status --porcelain` line count). 0 = clean.
    pub dirty_count: u32,
}

/// How long a `git_info` answer stays fresh per cwd. The Files panel re-polls
/// `git_info` PER TILE every 5s (plus a burst on window focus), so without a
/// cache M tiles on the same cwd each spawned their own `wsl.exe` every poll —
/// the storm that pinned the Tokio workers and froze the UI. A TTL just under
/// the 5s poll collapses every tile sharing a cwd (and the focus re-poll burst)
/// onto ONE `wsl.exe` invocation per cwd per poll cycle. A few seconds of
/// staleness on a branch/dirty-count is invisible in the panel header.
const GIT_INFO_TTL: Duration = Duration::from_millis(3500);

/// Per-cwd cache of the last `git_info` answer + when it was computed, shared
/// across all tiles/windows so rapid same-cwd re-polls collapse onto one real
/// `wsl.exe` invocation per [`GIT_INFO_TTL`] (mirrors `recent.rs`'s TTL cache).
/// Keyed by the raw `cwd` string the frontend passes (tiles for the same project
/// pass an identical cwd, so they share an entry). `Instant`, never wall-clock.
static GIT_INFO_CACHE: LazyLock<Mutex<HashMap<String, (Instant, GitInfo)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop any cached `git_info` for `cwd` so the next poll re-runs git. Called after
/// a mutation (commit / worktree add+remove) that changes what `git_info` reports —
/// otherwise the TTL cache would serve a stale answer (e.g. the pre-commit dirty
/// count) for up to `GIT_INFO_TTL` after the change.
fn invalidate_git_info_cache(cwd: &str) {
    if let Ok(mut guard) = GIT_INFO_CACHE.lock() {
        cache_invalidate(&mut guard, cwd);
    }
}

// ---------------------------------------------------------------------------
// Pure TTL-cache seams (unit-tested on Linux). These operate on an explicit map +
// `now`/`ttl` so the freshness/invalidation logic is testable WITHOUT the global
// static or wall-clock. Production wires them to `GIT_INFO_CACHE`, `Instant::now()`
// and `GIT_INFO_TTL`, so behavior is byte-identical to the inlined logic they
// replace. A flipped `<` here (serving stale data) is exactly what these catch.
// ---------------------------------------------------------------------------

/// Look up a still-fresh cached `GitInfo` for `cwd`: returns a clone iff an entry
/// exists AND its age (`now - stored_instant`) is strictly less than `ttl`. A
/// stale (age >= ttl) or absent entry yields `None`. Pure: no static, no clock.
fn cache_lookup(
    map: &HashMap<String, (Instant, GitInfo)>,
    cwd: &str,
    now: Instant,
    ttl: Duration,
) -> Option<GitInfo> {
    map.get(cwd)
        .filter(|(at, _)| now.duration_since(*at) < ttl)
        .map(|(_, info)| info.clone())
}

/// Store `info` for `cwd` stamped at `now` (the put half of the cache). Pure.
fn cache_store(
    map: &mut HashMap<String, (Instant, GitInfo)>,
    cwd: String,
    now: Instant,
    info: GitInfo,
) {
    map.insert(cwd, (now, info));
}

/// Drop any entry for `cwd` (the invalidation half). Pure.
fn cache_invalidate(map: &mut HashMap<String, (Instant, GitInfo)>, cwd: &str) {
    map.remove(cwd);
}

impl GitInfo {
    /// The "not a git repo" answer the commands fall back to (best-effort: a
    /// missing dir, a non-repo, or git not on PATH all collapse to this).
    fn not_repo() -> Self {
        Self {
            is_repo: false,
            branch: None,
            worktree_root: None,
            is_linked_worktree: false,
            dirty_count: 0,
        }
    }
}

/// One entry from `git worktree list --porcelain` (WS-4). Serialized camelCase to
/// mirror the TS `WorktreeInfo` interface in `src/ipc/git.ts`. `path` is the
/// worktree's working-tree dir (a POSIX path inside WSL); `branch` is the short
/// branch name (e.g. `feat/x`) or `None` on a detached/bare entry; `is_linked` is
/// true for every entry except the main worktree (the first one git reports).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    /// Absolute working-tree path of this worktree (POSIX inside WSL).
    pub path: String,
    /// Short branch name checked out here, or `None` (detached / bare).
    pub branch: Option<String>,
    /// True for a linked worktree (`git worktree add`); false for the main one.
    pub is_linked: bool,
}

// ---------------------------------------------------------------------------
// Platform plumbing: run a `git -C <cwd> <args...>` invocation and capture
// stdout. On unix this is a direct spawn; on Windows it shells into WSL.
// ---------------------------------------------------------------------------

/// Run `git <args...>` against `cwd` and return `(success, stdout, stderr)`.
///
/// unix: spawn `git` directly with `.current_dir(cwd)`.
///
/// Windows: spawn `wsl.exe -d <distro> --cd <cwd> -e git <args...>` with the
/// console window suppressed (`CREATE_NO_WINDOW`, mirroring files.rs/tmux.rs).
/// We rely on `--cd` rather than `git -C` so the working dir is set by WSL itself,
/// and `-e` (exec) runs git directly so no shell ever re-parses the args. `cwd`
/// here is a native POSIX path (`/home/...`); the desktop app hands us those.
fn run_git(cwd: &str, args: &[&str]) -> Result<(bool, String, String), String> {
    let output = build_git_command(cwd, args)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok((output.status.success(), stdout, stderr))
}

/// Build the `git` command for the current platform (see [`run_git`]).
#[cfg(not(windows))]
fn build_git_command(cwd: &str, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.current_dir(cwd);
    c.args(args);
    c
}

#[cfg(windows)]
fn build_git_command(cwd: &str, args: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;
    let distro = crate::files::host_distro();
    let mut c = Command::new("wsl.exe");
    // `wsl.exe -d <distro> --cd <posix-cwd> -e git <args...>`. `--cd` sets the
    // working dir inside the distro; `-e` (exec) runs git DIRECTLY with argv
    // preserved. With a bare `--`, wsl.exe re-joins the tail and routes it
    // through the user's default login shell (zsh here), which re-splits and
    // EXPANDS it — an arg containing `$`/backticks (e.g. a commit message) gets
    // shell-expanded (injection-shaped) and its quoting mangled. `-e` keeps each
    // arg (incl. the message) its own argv entry — see the detailed note on
    // tmux.rs `pane_info_command`.
    c.arg("-d")
        .arg(&distro)
        .arg("--cd")
        .arg(cwd)
        .arg("-e")
        .arg("git")
        .args(args);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see files.rs/tmux.rs)
    c
}

// ---------------------------------------------------------------------------
// One-shot `git_info` collection: ONE shell invocation computes everything.
//
// The Files panel polls `git_info` per tile every 5s (+ a focus burst). The old
// path made SIX sequential `run_git` calls, and on Windows each one spawned its
// own blocking `wsl.exe` on the Tokio executor — 6×M `wsl.exe` spawns every 5s,
// pinning worker threads. We collapse those six into a SINGLE `bash -lc` script
// (one `wsl.exe` on Windows; one direct `bash` on unix) that runs every git
// query in one shell and prints `key<TAB>value` lines we parse back into a
// `GitInfo`. The exact per-field semantics of the old six-call path are
// preserved by [`parse_git_info_output`].
// ---------------------------------------------------------------------------

/// The shell script run ONCE per `git_info`. It emits tab-delimited `key\tvalue`
/// lines (one per line; values are single-line git answers) that
/// [`parse_git_info_output`] maps back onto `GitInfo`:
///   - `inside\t<true|false|>` — `rev-parse --is-inside-work-tree` (the gate);
///   - `branch\t<name>`        — `rev-parse --abbrev-ref HEAD` (`HEAD` => detached);
///   - `toplevel\t<path>`      — `rev-parse --show-toplevel` (worktree root);
///   - `gitdir\t<path>`        — `rev-parse --git-dir`;
///   - `commondir\t<path>`     — `rev-parse --git-common-dir`;
///   - `dirty\t<n>`            — `git status --porcelain | wc -l` (dirty count).
/// We short-circuit to ONLY the `inside` line when not in a work tree, so a
/// non-repo collapses to `is_repo:false` exactly like the old cheap gate did.
/// Every git invocation is silenced (`2>/dev/null`) so stderr never pollutes the
/// parseable stdout; a failed query simply omits/blank-values its line, which the
/// parser already treats as "absent" (matching the old per-call `_ => None`).
const GIT_INFO_SCRIPT: &str = "\
inside=$(git rev-parse --is-inside-work-tree 2>/dev/null); \
printf 'inside\\t%s\\n' \"$inside\"; \
if [ \"$inside\" = true ]; then \
printf 'branch\\t%s\\n' \"$(git rev-parse --abbrev-ref HEAD 2>/dev/null)\"; \
printf 'toplevel\\t%s\\n' \"$(git rev-parse --show-toplevel 2>/dev/null)\"; \
printf 'gitdir\\t%s\\n' \"$(git rev-parse --git-dir 2>/dev/null)\"; \
printf 'commondir\\t%s\\n' \"$(git rev-parse --git-common-dir 2>/dev/null)\"; \
printf 'dirty\\t%s\\n' \"$(git status --porcelain 2>/dev/null | wc -l)\"; \
fi";

/// Run [`GIT_INFO_SCRIPT`] in ONE shell against `cwd`, returning its stdout.
///
/// unix: spawn `bash -lc <script>` with `.current_dir(cwd)`.
///
/// Windows: spawn `wsl.exe -d <distro> --cd <cwd> -e bash -lc <script>` with the
/// console window suppressed (`CREATE_NO_WINDOW`, matching `run_git`). The script
/// is a SINGLE argv arg (passed straight to `-lc` thanks to `-e`); it contains no
/// caller-interpolated data (the cwd is set by `--cd` / `current_dir`, never
/// spliced into the script text), so the trailing-arg mangling that bit the
/// commit path can't apply here. Returns `Err` only on a genuine spawn failure;
/// a non-repo just yields the short-circuited `inside\tfalse` stdout.
fn run_git_info_script(cwd: &str) -> Result<String, String> {
    let output = build_git_info_command(cwd)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Build the one-shot `bash -lc <script>` command for the current platform.
#[cfg(not(windows))]
fn build_git_info_command(cwd: &str) -> Command {
    let mut c = Command::new("bash");
    c.current_dir(cwd);
    c.arg("-lc").arg(GIT_INFO_SCRIPT);
    c
}

#[cfg(windows)]
fn build_git_info_command(cwd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let distro = crate::files::host_distro();
    let mut c = Command::new("wsl.exe");
    // `wsl.exe -d <distro> --cd <posix-cwd> -e bash -lc '<script>'`. `--cd` sets
    // the working dir inside the distro (NOT interpolated into the script). `-e`
    // (exec) hands the script to bash VERBATIM as one argv entry. With a bare
    // `--`, wsl.exe re-joins the tail through the user's default login shell
    // (zsh), which expands the script's `$inside`/`$( )` BEFORE bash runs — every
    // query got answered at zsh level, `inside` printed empty, and git_info
    // collapsed to not_repo for every real repo (v0.3.27's silently blank
    // branch/worktree titles). A login shell is fine here: the script is our own
    // and self-contained — see the detailed note on tmux.rs `pane_info_command`.
    c.arg("-d")
        .arg(&distro)
        .arg("--cd")
        .arg(cwd)
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(GIT_INFO_SCRIPT);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see files.rs/tmux.rs)
    c
}

/// Parse the [`GIT_INFO_SCRIPT`] stdout (tab-delimited `key\tvalue` lines) back
/// into a [`GitInfo`], preserving the EXACT semantics of the old six-call path:
///   - `inside` != `true`            -> `GitInfo::not_repo()` (`is_repo:false`);
///   - `branch` == `HEAD` or empty   -> `branch: None` (detached HEAD);
///   - `toplevel` empty              -> `worktree_root: None`;
///   - linked iff `gitdir` != `commondir` (both present), via `is_linked_worktree`;
///   - `dirty` parsed as the porcelain line count (absent/garbage -> 0).
/// Unknown keys and malformed lines are ignored. A blank value for any field is
/// treated as "absent", matching the old per-call `_ => None` fallbacks.
fn parse_git_info_output(stdout: &str) -> GitInfo {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('\t') {
            fields.insert(key.trim(), value.trim_end_matches(['\r', '\n']));
        }
    }

    // The gate: only an explicit `true` means we're inside a work tree.
    if fields.get("inside").map(|v| v.trim()) != Some("true") {
        return GitInfo::not_repo();
    }

    // Branch: map detached (`HEAD`) and empty to None, like `--abbrev-ref HEAD`.
    let branch = fields
        .get("branch")
        .and_then(|v| first_line_opt(v))
        .filter(|b| b != "HEAD");

    let worktree_root = fields.get("toplevel").and_then(|v| first_line_opt(v));

    let git_dir = fields.get("gitdir").and_then(|v| first_line_opt(v));
    let git_common_dir = fields.get("commondir").and_then(|v| first_line_opt(v));
    let is_linked_worktree = is_linked_worktree(git_dir.as_deref(), git_common_dir.as_deref());

    // Dirty count is already `git status --porcelain | wc -l` from the script.
    let dirty_count = fields
        .get("dirty")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);

    GitInfo {
        is_repo: true,
        branch,
        worktree_root,
        is_linked_worktree,
        dirty_count,
    }
}

// ---------------------------------------------------------------------------
// Pure parsing helpers (unit-tested on Linux).
// ---------------------------------------------------------------------------

/// Trim a single-line `git rev-parse` answer, returning `None` if empty.
fn first_line_opt(stdout: &str) -> Option<String> {
    let s = stdout.lines().next().unwrap_or("").trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Decide whether a working tree is a *linked* worktree from the two rev-parse
/// answers `--git-dir` and `--git-common-dir`. For the main worktree these point
/// at the same place (`.git`); for a linked worktree the per-worktree git dir is
/// `<common>/worktrees/<name>` — different from the common dir. We compare the
/// trimmed strings; if either is missing we conservatively say "not linked".
fn is_linked_worktree(git_dir: Option<&str>, git_common_dir: Option<&str>) -> bool {
    match (git_dir, git_common_dir) {
        (Some(g), Some(c)) => g.trim() != c.trim(),
        _ => false,
    }
}

/// Parse `git worktree list --porcelain` into a list of [`WorktreeInfo`]. The
/// porcelain format emits one record per worktree, records separated by a blank
/// line; within a record each attribute is a line:
/// ```text
/// worktree /home/u/repo
/// HEAD <sha>
/// branch refs/heads/main
///
/// worktree /home/u/repo-feat
/// HEAD <sha>
/// branch refs/heads/feat/x
/// ```
/// A detached worktree omits the `branch` line (and carries a `detached` line); a
/// bare repo carries a `bare` line. We map `branch refs/heads/<name>` to the short
/// `<name>` and leave `branch: None` otherwise. The FIRST entry git reports is the
/// main worktree (`is_linked: false`); every later entry is a linked worktree.
fn parse_worktree_list(stdout: &str) -> Vec<WorktreeInfo> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;

    // Flush the in-progress record (if it has a path) into `out`, marking it
    // linked iff it isn't the first record we've seen.
    let flush = |path: &mut Option<String>, branch: &mut Option<String>, out: &mut Vec<WorktreeInfo>| {
        if let Some(p) = path.take() {
            let is_linked = !out.is_empty();
            out.push(WorktreeInfo {
                path: p,
                branch: branch.take(),
                is_linked,
            });
        } else {
            // No path => nothing to flush, but still clear any stray branch.
            *branch = None;
        }
    };

    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            // Blank line: record separator.
            flush(&mut path, &mut branch, &mut out);
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            // A new `worktree` line begins a new record; flush any prior one that
            // wasn't terminated by a blank line (defensive — git always blanks).
            flush(&mut path, &mut branch, &mut out);
            path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            // `branch refs/heads/<name>` -> short `<name>`; tolerate a bare ref.
            let short = b
                .trim()
                .strip_prefix("refs/heads/")
                .unwrap_or_else(|| b.trim())
                .to_string();
            if !short.is_empty() {
                branch = Some(short);
            }
        }
        // Other attribute lines (HEAD/detached/bare/locked/prunable) don't affect
        // the fields we surface.
    }
    // Flush a trailing record with no terminating blank line.
    flush(&mut path, &mut branch, &mut out);
    out
}

/// Extract the branch name a `git worktree add` failure says is already checked
/// out elsewhere, so the surfaced error can name it. git phrases this as e.g.
/// `fatal: 'feat/x' is already checked out at '/path'`. Returns the unquoted
/// branch if the message matches, else `None`.
fn already_checked_out_branch(stderr: &str) -> Option<String> {
    let lower = stderr.to_lowercase();
    if !lower.contains("already checked out") && !lower.contains("already used by worktree") {
        return None;
    }
    // Pull the first single-quoted token (git quotes the branch/ref first).
    let start = stderr.find('\'')? + 1;
    let rest = &stderr[start..];
    let end = rest.find('\'')?;
    let name = &rest[..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Build the `git worktree add` argv for the SMART branch behavior (WS-9), given
/// the trimmed `path` and the caller's `branch` choice plus whether that local
/// branch already exists (the existence check — a real `git show-ref` — lives in
/// [`worktree_add`]; this stays pure so it's unit-testable without git):
///   - `branch = None`                  -> `["worktree","add",path]` (git's default:
///     creates a branch named after the path's final component);
///   - `branch = Some(b)`, exists       -> `["worktree","add",path,b]` (check the
///     existing branch out — the original WS-4 behavior);
///   - `branch = Some(b)`, NOT exists   -> `["worktree","add",path,"-b",b]` (create
///     the branch and check it out).
/// Each element is its own argv entry (no shell interpolation), so paths/branches
/// with metacharacters — and branch names containing `/` (e.g. `feature/login`) —
/// are passed verbatim to git.
fn worktree_add_args<'a>(path: &'a str, branch: Option<&'a str>, branch_exists: bool) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec!["worktree", "add", path];
    if let Some(b) = branch {
        if branch_exists {
            // Existing branch: check it out (`git worktree add <path> <b>`).
            args.push(b);
        } else {
            // New branch: create it (`git worktree add <path> -b <b>`).
            args.push("-b");
            args.push(b);
        }
    }
    args
}

// ---------------------------------------------------------------------------
// Tauri commands (registered in lib.rs; mirrored in src/ipc/git.ts)
// ---------------------------------------------------------------------------

/// Report git facts for `cwd` (branch / worktree root / linked-worktree / dirty
/// count). Best-effort: a non-repo, missing dir, or absent git all yield
/// `is_repo: false`. Never returns `Err` for a non-repo — only a genuinely
/// unexpected spawn/join failure is surfaced (so the UI can stay quiet).
///
/// PERF (the freeze fix): this used to make SIX sequential blocking `run_git`
/// calls — on Windows six separate `wsl.exe` spawns — directly on the async
/// executor, per tile, every 5s poll (+ a focus burst). It now does ONE of three
/// cheap things: (1) returns a fresh cached answer for this cwd, OR (2) runs a
/// SINGLE one-shot shell script ([`GIT_INFO_SCRIPT`]) — one `wsl.exe` total —
/// inside `spawn_blocking` so the blocking IO never pins a Tokio worker, then
/// caches it. Many tiles polling the same cwd (and the focus re-poll burst) thus
/// collapse onto one `wsl.exe` per cwd per [`GIT_INFO_TTL`].
#[tauri::command]
pub async fn git_info(cwd: String) -> Result<GitInfo, String> {
    // (1) Serve a fresh-enough cached answer for this cwd. The lock is held only
    // to clone the cached `GitInfo`, never across the spawn below, so many tiles
    // polling the same cwd within the TTL all return without spawning anything.
    if let Some(cached) = GIT_INFO_CACHE
        .lock()
        .ok()
        .and_then(|g| cache_lookup(&g, &cwd, Instant::now(), GIT_INFO_TTL))
    {
        return Ok(cached);
    }

    // (2) Cache miss / stale: run the single one-shot script off the async
    // runtime. The blocking work (the `wsl.exe`/`bash` spawn + parse) lives in a
    // closure that captures only OWNED data (the cloned `cwd`) — no `&State` is
    // held across the await — so it can't pin a worker thread.
    let cwd_for_blocking = cwd.clone();
    let info = tauri::async_runtime::spawn_blocking(move || compute_git_info(&cwd_for_blocking))
        .await
        .map_err(|e| format!("git_info task failed: {e}"))?;

    // Cache the fresh answer for this cwd so sibling tiles + the focus burst hit (1).
    if let Ok(mut guard) = GIT_INFO_CACHE.lock() {
        cache_store(&mut guard, cwd, Instant::now(), info.clone());
    }
    Ok(info)
}

/// Run the one-shot [`GIT_INFO_SCRIPT`] for `cwd` and parse it into a [`GitInfo`] —
/// the "scan" half shared by the async [`git_info`] command (inside its
/// `spawn_blocking`) and the sync [`git_info_cached`] control core. A spawn failure
/// (no git / no wsl.exe / unreadable dir) degrades to the "not a repo" answer,
/// best-effort exactly like the old per-call path. Pure blocking IO; no cache.
fn compute_git_info(cwd: &str) -> GitInfo {
    let stdout = run_git_info_script(cwd).unwrap_or_default();
    parse_git_info_output(&stdout)
}

/// SYNC `git_info` for `cwd` — the core of [`git_info`] minus the async/`spawn_blocking`
/// wrapper. The control channel calls this (server-split M3) to serve the daemon's git
/// awareness over the socket, so a thin client gets a project's branch / worktree root /
/// linked flag / dirty count remotely. Reuses [`GIT_INFO_CACHE`] (the freeze-fix
/// per-cwd TTL cache) via the same [`cache_lookup`]/[`cache_store`] seams as the async
/// command, so local + remote per-tile polls collapse onto one `wsl.exe`/`git` spawn
/// per cwd per [`GIT_INFO_TTL`]. Safe on a (blocking) control connection thread — the
/// script spawn is blocking IO.
pub fn git_info_cached(cwd: &str) -> GitInfo {
    // (1) Fresh cached answer for this cwd (same fast-path as the async command).
    if let Some(cached) = GIT_INFO_CACHE
        .lock()
        .ok()
        .and_then(|g| cache_lookup(&g, cwd, Instant::now(), GIT_INFO_TTL))
    {
        return cached;
    }
    // (2) Cache miss / stale: run the one-shot script synchronously, then cache it.
    let info = compute_git_info(cwd);
    if let Ok(mut guard) = GIT_INFO_CACHE.lock() {
        cache_store(&mut guard, cwd.to_string(), Instant::now(), info.clone());
    }
    info
}

/// Stage all changes (`git add -A`) and commit them with `message`. Returns the
/// new commit's short hash on success, or the trimmed git output. Rejects an
/// empty/whitespace-only message before touching the repo. The message is passed
/// as a distinct argv arg to `git commit -m`, never interpolated into a shell
/// string, so metacharacters are safe.
#[tauri::command]
pub async fn git_commit(cwd: String, message: String) -> Result<String, String> {
    // Multiple blocking `run_git` spawns (on Windows each a `wsl.exe` child):
    // `add -A`, `commit -m`, then `rev-parse`. Run the whole sequence off the
    // Tokio executor so it can't pin a worker thread (mirrors `git_info`). The
    // closure owns `cwd`/`message` (moved in), so it's `'static + Send`; no
    // `&State` is captured.
    tauri::async_runtime::spawn_blocking(move || {
        let msg = message.trim();
        if msg.is_empty() {
            return Err("commit message is empty".to_string());
        }

        // Stage everything (new, modified, deleted).
        match run_git(&cwd, &["add", "-A"]) {
            Ok((true, _, _)) => {}
            Ok((_, _, stderr)) => {
                return Err(format!("git add failed: {}", stderr.trim()));
            }
            Err(e) => return Err(e),
        }

        // Commit. `-m <msg>` is two argv entries; the message is never shell-parsed.
        let (ok, stdout, stderr) = run_git(&cwd, &["commit", "-m", msg])?;
        if !ok {
            // Surface git's own message (e.g. "nothing to commit, working tree clean").
            let detail = if !stderr.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            };
            return Err(format!("git commit failed: {detail}"));
        }

        // The dirty count just changed — drop the stale cache entry so the UI's
        // immediate post-commit refresh re-runs git instead of serving the old count.
        invalidate_git_info_cache(&cwd);

        // Report the new short hash so the UI can confirm the commit.
        match run_git(&cwd, &["rev-parse", "--short", "HEAD"]) {
            Ok((true, out, _)) => {
                Ok(first_line_opt(&out).unwrap_or_else(|| stdout.trim().to_string()))
            }
            // Commit succeeded but we couldn't read the hash — return git's output.
            _ => Ok(stdout.trim().to_string()),
        }
    })
    .await
    .map_err(|e| format!("git_commit task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Worktree commands (WS-4). Mirror `git_commit`'s exec pattern exactly: shell out
// to git via `run_git` (wsl.exe `--cd` on Windows, direct `.current_dir` on unix),
// passing each argument as its own argv entry so paths/branches with shell
// metacharacters are safe. They do NOT route through the agent protocol.
// ---------------------------------------------------------------------------

/// List the worktrees attached to the repository containing `cwd` (parses
/// `git worktree list --porcelain`). Returns the main worktree first
/// (`isLinked: false`) followed by every linked worktree. A non-repo (or an
/// unreadable dir) yields an empty list rather than erroring, so the UI can stay
/// quiet; only a genuine spawn failure surfaces as `Err`.
#[tauri::command]
pub async fn git_worktree_list(cwd: String) -> Result<Vec<WorktreeInfo>, String> {
    // `run_git` is a blocking spawn (a `wsl.exe` child on Windows); run it off the
    // Tokio executor (the owned `cwd` is moved into the `'static + Send` closure).
    tauri::async_runtime::spawn_blocking(move || {
        match run_git(&cwd, &["worktree", "list", "--porcelain"]) {
            Ok((true, stdout, _)) => Ok(parse_worktree_list(&stdout)),
            // Not a repo / no worktrees / git unavailable: empty list (best-effort).
            Ok((false, _, _)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| format!("git_worktree_list task failed: {e}"))?
}

/// Create (or check out into) a worktree at `path` for the repo containing `cwd`,
/// with SMART branch handling (WS-9, option A):
///   - with `branch` that EXISTS locally: checks that branch out at `path`
///     (`git worktree add <path> <branch>` — the original WS-4 behavior); git still
///     refuses a branch already used by another worktree (named clearly below);
///   - with `branch` that does NOT exist: CREATES it and checks it out
///     (`git worktree add <path> -b <branch>`);
///   - without `branch`: git creates a new branch named after the final path
///     component (its default behavior) and checks it out at `path`.
/// Returns git's output on success. On the common "branch already checked out
/// elsewhere" failure we surface a CLEAR, named error (the path is a POSIX path
/// inside WSL; each arg is its own argv entry, so spaces/metacharacters are safe).
#[tauri::command]
pub async fn git_worktree_add(
    cwd: String,
    path: String,
    branch: Option<String>,
) -> Result<String, String> {
    // `worktree_add` runs several blocking `run_git` spawns (show-ref + the add,
    // each a `wsl.exe` child on Windows). Run it off the Tokio executor; the owned
    // args are moved into the `'static + Send` closure.
    tauri::async_runtime::spawn_blocking(move || worktree_add(&cwd, &path, branch.as_deref()))
        .await
        .map_err(|e| format!("git_worktree_add task failed: {e}"))?
}

/// Synchronous core of [`git_worktree_add`], shared with the MCP control channel
/// (`control::create_worktree`) so both call exactly one implementation.
pub(crate) fn worktree_add(cwd: &str, path: &str, branch: Option<&str>) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("worktree path is empty".to_string());
    }

    // Normalize the branch: a `None`/empty branch keeps git's default behavior.
    let branch_trimmed = branch.map(str::trim).filter(|b| !b.is_empty());

    // SMART branch handling (WS-9): when a branch is named, check whether the local
    // branch already exists so we pick checkout (`<b>`) vs create (`-b <b>`). The
    // check is `git show-ref --verify --quiet refs/heads/<b>` — exit 0 means the
    // ref exists. Branch names may contain `/` (e.g. `feature/login`); the full ref
    // `refs/heads/feature/login` is passed as its own argv entry, so it's safe.
    // A `None` branch skips the check entirely (git's path-derived default).
    let branch_exists = if let Some(b) = branch_trimmed {
        let ref_name = format!("refs/heads/{b}");
        matches!(
            run_git(cwd, &["show-ref", "--verify", "--quiet", &ref_name]),
            Ok((true, _, _))
        )
    } else {
        false
    };

    // Build argv from the pure decision helper (checkout vs `-b` create vs default).
    let args = worktree_add_args(path, branch_trimmed, branch_exists);

    let (ok, stdout, stderr) = run_git(cwd, &args)?;
    if !ok {
        // Name the branch git says is already checked out elsewhere, when it is.
        if let Some(b) = already_checked_out_branch(&stderr) {
            return Err(format!(
                "git worktree add failed: branch '{b}' is already checked out in \
                 another worktree (a branch can be checked out in only one \
                 worktree at a time). Pick a different branch or remove the other \
                 worktree first."
            ));
        }
        // A leftover directory at the target path (a stale dir, or one left by a
        // prior failed/aborted run) makes git refuse with a raw "already exists".
        // Surface an ACTIONABLE message — the user just needs to remove it or pick
        // a different branch name (which derives a different dir).
        if stderr.to_lowercase().contains("already exists") {
            return Err(format!(
                "git worktree add failed: a directory already exists at '{path}'. \
                 Remove that leftover directory, or pick a different branch name, \
                 and try again."
            ));
        }
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        return Err(format!("git worktree add failed: {detail}"));
    }
    // The new worktree dir is now a repo (and the source repo's worktree set
    // changed) — drop any cached git_info for both so a fresh poll is accurate.
    invalidate_git_info_cache(cwd);
    invalidate_git_info_cache(path);
    Ok(stdout.trim().to_string())
}

/// Remove the worktree at `path` from the repo containing `cwd`
/// (`git worktree remove [--force] <path>`). git refuses to remove a worktree with
/// uncommitted changes unless `force` is set; we surface git's own message on
/// failure. Idempotency is left to git (a missing worktree is an error, reported
/// verbatim). Each arg is its own argv entry.
#[tauri::command]
pub async fn git_worktree_remove(
    cwd: String,
    path: String,
    force: Option<bool>,
) -> Result<(), String> {
    // `worktree_remove` is a blocking `run_git` spawn (a `wsl.exe` child on
    // Windows). Run it off the Tokio executor; the owned args are moved into the
    // `'static + Send` closure.
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || worktree_remove(&cwd, &path, force))
        .await
        .map_err(|e| format!("git_worktree_remove task failed: {e}"))?
}

/// Synchronous core of [`git_worktree_remove`], shared with the MCP control
/// channel (`control::remove_worktree`).
pub(crate) fn worktree_remove(cwd: &str, path: &str, force: bool) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("worktree path is empty".to_string());
    }
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path);

    let (ok, stdout, stderr) = run_git(cwd, &args)?;
    if !ok {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        return Err(format!("git worktree remove failed: {detail}"));
    }
    // The worktree is gone — drop any cached git_info for it and the source repo.
    invalidate_git_info_cache(cwd);
    invalidate_git_info_cache(path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (pure parsing helpers; runnable on Linux).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_opt_trims_and_handles_empty() {
        assert_eq!(first_line_opt(""), None);
        assert_eq!(first_line_opt("   \n"), None);
        assert_eq!(first_line_opt("main\n"), Some("main".to_string()));
        // Only the first line is taken (rev-parse answers are single-line).
        assert_eq!(
            first_line_opt("feat/git-panel\nignored\n"),
            Some("feat/git-panel".to_string())
        );
    }

    #[test]
    fn linked_worktree_detection() {
        // Main worktree: git-dir == git-common-dir.
        assert!(!is_linked_worktree(Some("/repo/.git"), Some("/repo/.git")));
        // Linked worktree: per-worktree dir differs from the common dir.
        assert!(is_linked_worktree(
            Some("/repo/.git/worktrees/wt"),
            Some("/repo/.git")
        ));
        // Whitespace is tolerated on either side.
        assert!(!is_linked_worktree(Some(" .git \n"), Some(".git")));
        // Missing either answer is conservatively "not linked".
        assert!(!is_linked_worktree(None, Some("/repo/.git")));
        assert!(!is_linked_worktree(Some("/repo/.git"), None));
    }

    #[test]
    fn parse_git_info_output_non_repo() {
        // The short-circuited script output when not inside a work tree: only the
        // `inside` line, valued anything but `true` (here git printed nothing).
        let info = parse_git_info_output("inside\t\n");
        assert_eq!(info, GitInfo::not_repo());
        // A literal `false` (some git builds) is likewise "not a repo".
        assert_eq!(parse_git_info_output("inside\tfalse\n"), GitInfo::not_repo());
        // Completely empty stdout (total spawn weirdness) -> not a repo.
        assert_eq!(parse_git_info_output(""), GitInfo::not_repo());
    }

    #[test]
    fn parse_git_info_output_main_worktree() {
        // Main worktree: gitdir == commondir -> not linked; a real branch + dirty.
        let out = "\
inside\ttrue
branch\tmain
toplevel\t/home/u/repo
gitdir\t/home/u/repo/.git
commondir\t/home/u/repo/.git
dirty\t3
";
        let info = parse_git_info_output(out);
        assert!(info.is_repo);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.worktree_root.as_deref(), Some("/home/u/repo"));
        assert!(!info.is_linked_worktree);
        assert_eq!(info.dirty_count, 3);
    }

    #[test]
    fn parse_git_info_output_linked_worktree() {
        // Linked worktree: per-worktree gitdir differs from the common dir; clean.
        let out = "\
inside\ttrue
branch\tfeat/x
toplevel\t/home/u/repo-feat
gitdir\t/home/u/repo/.git/worktrees/repo-feat
commondir\t/home/u/repo/.git
dirty\t0
";
        let info = parse_git_info_output(out);
        assert!(info.is_repo);
        assert_eq!(info.branch.as_deref(), Some("feat/x"));
        assert_eq!(info.worktree_root.as_deref(), Some("/home/u/repo-feat"));
        assert!(info.is_linked_worktree, "gitdir != commondir => linked");
        assert_eq!(info.dirty_count, 0);
    }

    #[test]
    fn parse_git_info_output_detached_head_maps_branch_to_none() {
        // `--abbrev-ref HEAD` is the literal `HEAD` on a detached checkout — the
        // parser must map that (and an empty branch) to None, like the old path.
        let out = "\
inside\ttrue
branch\tHEAD
toplevel\t/home/u/repo
gitdir\t/home/u/repo/.git
commondir\t/home/u/repo/.git
dirty\t0
";
        assert_eq!(parse_git_info_output(out).branch, None);
        // An empty branch value is also None (git failed/blanked that line).
        let out_blank = "inside\ttrue\nbranch\t\ntoplevel\t/r\ndirty\t0\n";
        assert_eq!(parse_git_info_output(out_blank).branch, None);
    }

    #[test]
    fn parse_git_info_output_tolerates_missing_and_garbage_fields() {
        // Inside a repo but several lines absent / a garbage dirty value: missing
        // fields collapse to None (matching the old per-call `_ => None`) and an
        // unparseable dirty count falls back to 0.
        let out = "inside\ttrue\ndirty\tnope\n";
        let info = parse_git_info_output(out);
        assert!(info.is_repo);
        assert_eq!(info.branch, None);
        assert_eq!(info.worktree_root, None);
        assert!(!info.is_linked_worktree); // both gitdir/commondir absent
        assert_eq!(info.dirty_count, 0); // "nope" -> 0
    }

    #[test]
    fn not_repo_is_empty() {
        let info = GitInfo::not_repo();
        assert!(!info.is_repo);
        assert_eq!(info.branch, None);
        assert_eq!(info.worktree_root, None);
        assert!(!info.is_linked_worktree);
        assert_eq!(info.dirty_count, 0);
    }

    #[test]
    fn parse_worktree_list_marks_main_then_linked() {
        let out = "\
worktree /home/u/repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /home/u/repo-feat
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feat/x
";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[0].path, "/home/u/repo");
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(!wts[0].is_linked, "first entry is the main worktree");
        assert_eq!(wts[1].path, "/home/u/repo-feat");
        assert_eq!(wts[1].branch.as_deref(), Some("feat/x"));
        assert!(wts[1].is_linked, "later entries are linked worktrees");
    }

    #[test]
    fn parse_worktree_list_handles_detached_and_bare() {
        // A bare repo entry (no branch) then a detached worktree (HEAD + detached,
        // no branch line). Neither should carry a branch.
        let out = "\
worktree /home/u/repo.git
bare

worktree /home/u/detached
HEAD 3333333333333333333333333333333333333333
detached
";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[0].path, "/home/u/repo.git");
        assert_eq!(wts[0].branch, None);
        assert!(!wts[0].is_linked);
        assert_eq!(wts[1].path, "/home/u/detached");
        assert_eq!(wts[1].branch, None);
        assert!(wts[1].is_linked);
    }

    #[test]
    fn parse_worktree_list_empty_input() {
        assert!(parse_worktree_list("").is_empty());
        assert!(parse_worktree_list("\n\n").is_empty());
    }

    #[test]
    fn parse_worktree_list_trailing_record_without_blank() {
        // No terminating blank line on the last record (defensive flush).
        let out = "worktree /home/u/repo\nbranch refs/heads/dev";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].path, "/home/u/repo");
        assert_eq!(wts[0].branch.as_deref(), Some("dev"));
    }

    // -----------------------------------------------------------------------
    // TTL-cache seams (the GIT_INFO_CACHE perf change). These guard against the
    // silent-stale-data failure: a flipped TTL comparison or a forgotten
    // invalidation would serve an old branch/dirty-count with no other signal.
    // -----------------------------------------------------------------------

    /// A trivial non-default `GitInfo` so a served entry is distinguishable.
    fn sample_info() -> GitInfo {
        GitInfo {
            is_repo: true,
            branch: Some("main".to_string()),
            worktree_root: Some("/home/u/repo".to_string()),
            is_linked_worktree: false,
            dirty_count: 2,
        }
    }

    #[test]
    fn cache_lookup_serves_a_fresh_entry() {
        // (a) Age < TTL: the cached answer is returned verbatim.
        let ttl = Duration::from_millis(3500);
        let now = Instant::now();
        let mut map: HashMap<String, (Instant, GitInfo)> = HashMap::new();
        // Stored 1s ago — well within a 3.5s TTL.
        map.insert(
            "/repo".to_string(),
            (now - Duration::from_secs(1), sample_info()),
        );
        assert_eq!(cache_lookup(&map, "/repo", now, ttl), Some(sample_info()));
        // A different cwd has no entry -> None (cache is keyed per cwd).
        assert_eq!(cache_lookup(&map, "/other", now, ttl), None);
    }

    #[test]
    fn cache_lookup_rejects_a_stale_entry() {
        // (b) Age > TTL: a stale entry is NOT served (forces a re-run). This is the
        // case a flipped `<`/`>=` comparison would silently break.
        let ttl = Duration::from_millis(3500);
        let now = Instant::now();
        let mut map: HashMap<String, (Instant, GitInfo)> = HashMap::new();
        // Stored 5s ago — older than the 3.5s TTL.
        map.insert(
            "/repo".to_string(),
            (now - Duration::from_secs(5), sample_info()),
        );
        assert_eq!(cache_lookup(&map, "/repo", now, ttl), None);
        // Boundary: age exactly == TTL is treated as stale (strict `<`).
        let mut at_boundary: HashMap<String, (Instant, GitInfo)> = HashMap::new();
        at_boundary.insert("/repo".to_string(), (now - ttl, sample_info()));
        assert_eq!(cache_lookup(&at_boundary, "/repo", now, ttl), None);
    }

    #[test]
    fn cache_store_then_lookup_round_trips() {
        // The put half stamps the entry at `now` so an immediate lookup is fresh.
        let ttl = Duration::from_millis(3500);
        let now = Instant::now();
        let mut map: HashMap<String, (Instant, GitInfo)> = HashMap::new();
        cache_store(&mut map, "/repo".to_string(), now, sample_info());
        assert_eq!(cache_lookup(&map, "/repo", now, ttl), Some(sample_info()));
    }

    #[test]
    fn cache_invalidate_drops_the_entry() {
        // (c) Invalidation (the commit/worktree path) removes the entry, so the
        // very next lookup misses and the caller re-runs git instead of serving the
        // pre-mutation answer. A forgotten invalidation would leave this fresh.
        let ttl = Duration::from_millis(3500);
        let now = Instant::now();
        let mut map: HashMap<String, (Instant, GitInfo)> = HashMap::new();
        cache_store(&mut map, "/repo".to_string(), now, sample_info());
        // Fresh before invalidation...
        assert!(cache_lookup(&map, "/repo", now, ttl).is_some());
        cache_invalidate(&mut map, "/repo");
        // ...gone after. Invalidating an absent key is a harmless no-op.
        assert_eq!(cache_lookup(&map, "/repo", now, ttl), None);
        cache_invalidate(&mut map, "/repo");
        assert_eq!(cache_lookup(&map, "/repo", now, ttl), None);
    }

    #[test]
    fn already_checked_out_branch_names_the_branch() {
        let stderr = "fatal: 'feat/x' is already checked out at '/home/u/repo-feat'\n";
        assert_eq!(
            already_checked_out_branch(stderr).as_deref(),
            Some("feat/x")
        );
        // The "already used by worktree" phrasing is also recognized.
        let alt = "fatal: 'main' is already used by worktree at '/home/u/repo'\n";
        assert_eq!(already_checked_out_branch(alt).as_deref(), Some("main"));
        // An unrelated failure returns None (so we fall back to git's message).
        assert_eq!(
            already_checked_out_branch("fatal: not a git repository"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // Smart worktree-add arg decision (WS-9, option A). The git existence check
    // lives in `worktree_add`; here we test ONLY the pure arg builder: a new
    // branch must use `-b`, an existing branch must use the bare checkout form
    // (no `-b`), and `None` must stay the path-derived default.
    // -----------------------------------------------------------------------

    #[test]
    fn worktree_add_args_new_branch_uses_dash_b() {
        // branch given + does NOT exist -> create it with `-b`.
        let args = worktree_add_args("/home/u/repo-feat", Some("feat/login"), false);
        assert_eq!(args, vec!["worktree", "add", "/home/u/repo-feat", "-b", "feat/login"]);
        // `-b` must come immediately before the branch name (git's create form).
        let b_idx = args.iter().position(|a| *a == "-b").expect("contains -b");
        assert_eq!(args[b_idx + 1], "feat/login");
    }

    #[test]
    fn worktree_add_args_existing_branch_checks_out_without_dash_b() {
        // branch given + already exists -> bare checkout (`<path> <branch>`, no `-b`).
        let args = worktree_add_args("/home/u/repo-feat", Some("feat/login"), true);
        assert_eq!(args, vec!["worktree", "add", "/home/u/repo-feat", "feat/login"]);
        assert!(!args.contains(&"-b"), "existing branch must not pass -b");
    }

    #[test]
    fn worktree_add_args_none_branch_is_bare_default() {
        // No branch -> `["worktree","add",path]`; git derives the branch from the
        // path's final component. The `branch_exists` flag is irrelevant here.
        let expected = vec!["worktree", "add", "/home/u/repo-feat"];
        assert_eq!(worktree_add_args("/home/u/repo-feat", None, false), expected);
        assert_eq!(worktree_add_args("/home/u/repo-feat", None, true), expected);
    }
}
