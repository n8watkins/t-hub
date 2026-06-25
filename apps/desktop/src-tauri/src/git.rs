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

use std::process::Command;

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

/// The WSL distro projects live in, as seen from the Windows host. Mirrors
/// `files.rs::host_distro` (replicated locally to stay in-lane this batch):
/// overridable via `T_HUB_DISTRO`, defaulting to the dev distro. Windows only.
#[cfg(windows)]
fn host_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Run `git <args...>` against `cwd` and return `(success, stdout, stderr)`.
///
/// unix: spawn `git` directly with `.current_dir(cwd)`.
///
/// Windows: spawn `wsl.exe -d <distro> --cd <cwd> -- git <args...>` with the
/// console window suppressed (`CREATE_NO_WINDOW`, mirroring files.rs/tmux.rs).
/// We rely on `--cd` rather than `git -C` so the working dir is set by WSL itself,
/// and we pass NO bash script (avoids the trailing-arg mangling caveat). `cwd`
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
    let distro = host_distro();
    let mut c = Command::new("wsl.exe");
    // `wsl.exe -d <distro> --cd <posix-cwd> -- git <args...>`. `--cd` sets the
    // working dir inside the distro; `--` ends wsl flags so everything after is
    // the command. git lives on the default PATH (/usr/bin), so no login shell —
    // and crucially NO `bash -lc '<script>'`, which can mangle a trailing arg
    // (the commit message). Each arg (incl. the message) is its own argv entry.
    c.arg("-d")
        .arg(&distro)
        .arg("--cd")
        .arg(cwd)
        .arg("--")
        .arg("git")
        .args(args);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see files.rs/tmux.rs)
    c
}

// ---------------------------------------------------------------------------
// Pure parsing helpers (unit-tested on Linux).
// ---------------------------------------------------------------------------

/// Count changed entries from `git status --porcelain` output: one entry per
/// non-empty line. Tolerates a trailing newline / blank lines.
fn parse_porcelain_count(stdout: &str) -> u32 {
    stdout.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

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
    let mut flush = |path: &mut Option<String>, branch: &mut Option<String>, out: &mut Vec<WorktreeInfo>| {
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

// ---------------------------------------------------------------------------
// Tauri commands (registered in lib.rs; mirrored in src/ipc/git.ts)
// ---------------------------------------------------------------------------

/// Report git facts for `cwd` (branch / worktree root / linked-worktree / dirty
/// count). Best-effort: a non-repo, missing dir, or absent git all yield
/// `is_repo: false`. Never returns `Err` for a non-repo — only for a genuinely
/// unexpected spawn failure is surfaced (so the UI can stay quiet).
#[tauri::command]
pub async fn git_info(cwd: String) -> Result<GitInfo, String> {
    // Cheap gate: is this even inside a work tree? If not (or git is unavailable),
    // collapse to the "not a repo" answer and render nothing in the UI.
    let inside = match run_git(&cwd, &["rev-parse", "--is-inside-work-tree"]) {
        Ok((true, out, _)) => out.trim() == "true",
        // A non-zero exit (the usual "not a git repository") or any spawn problem
        // is treated as "not a repo" — best-effort, the UI just shows nothing.
        Ok(_) => false,
        Err(_) => false,
    };
    if !inside {
        return Ok(GitInfo::not_repo());
    }

    // Branch: `--abbrev-ref HEAD` is `HEAD` on a detached checkout; map that to
    // None so the UI shows no branch rather than a misleading "HEAD".
    let branch = match run_git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok((true, out, _)) => first_line_opt(&out).filter(|b| b != "HEAD"),
        _ => None,
    };

    // Worktree root.
    let worktree_root = match run_git(&cwd, &["rev-parse", "--show-toplevel"]) {
        Ok((true, out, _)) => first_line_opt(&out),
        _ => None,
    };

    // Linked-worktree detection: compare --git-dir vs --git-common-dir.
    let git_dir = match run_git(&cwd, &["rev-parse", "--git-dir"]) {
        Ok((true, out, _)) => first_line_opt(&out),
        _ => None,
    };
    let git_common_dir = match run_git(&cwd, &["rev-parse", "--git-common-dir"]) {
        Ok((true, out, _)) => first_line_opt(&out),
        _ => None,
    };
    let linked = is_linked_worktree(git_dir.as_deref(), git_common_dir.as_deref());

    // Dirty count: one entry per porcelain line.
    let dirty_count = match run_git(&cwd, &["status", "--porcelain"]) {
        Ok((true, out, _)) => parse_porcelain_count(&out),
        _ => 0,
    };

    Ok(GitInfo {
        is_repo: true,
        branch,
        worktree_root,
        is_linked_worktree: linked,
        dirty_count,
    })
}

/// Stage all changes (`git add -A`) and commit them with `message`. Returns the
/// new commit's short hash on success, or the trimmed git output. Rejects an
/// empty/whitespace-only message before touching the repo. The message is passed
/// as a distinct argv arg to `git commit -m`, never interpolated into a shell
/// string, so metacharacters are safe.
#[tauri::command]
pub async fn git_commit(cwd: String, message: String) -> Result<String, String> {
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

    // Report the new short hash so the UI can confirm the commit.
    match run_git(&cwd, &["rev-parse", "--short", "HEAD"]) {
        Ok((true, out, _)) => Ok(first_line_opt(&out).unwrap_or_else(|| stdout.trim().to_string())),
        // Commit succeeded but we couldn't read the hash — return git's output.
        _ => Ok(stdout.trim().to_string()),
    }
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
    match run_git(&cwd, &["worktree", "list", "--porcelain"]) {
        Ok((true, stdout, _)) => Ok(parse_worktree_list(&stdout)),
        // Not a repo / no worktrees / git unavailable: empty list (best-effort).
        Ok((false, _, _)) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Create (or check out into) a worktree at `path` for the repo containing `cwd`.
/// Runs `git worktree add <path> [branch]`:
///   - with `branch`: checks that branch out at `path` (it must exist and not be
///     checked out elsewhere — git refuses a branch already used by a worktree);
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
    worktree_add(&cwd, &path, branch.as_deref())
}

/// Synchronous core of [`git_worktree_add`], shared with the MCP control channel
/// (`control::create_worktree`) so both call exactly one implementation.
pub(crate) fn worktree_add(cwd: &str, path: &str, branch: Option<&str>) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("worktree path is empty".to_string());
    }

    // Build argv: `worktree add <path> [branch]`. Each is a distinct argv entry.
    let mut args: Vec<&str> = vec!["worktree", "add", path];
    let branch_trimmed = branch.map(str::trim).filter(|b| !b.is_empty());
    if let Some(b) = branch_trimmed {
        args.push(b);
    }

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
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        return Err(format!("git worktree add failed: {detail}"));
    }
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
    worktree_remove(&cwd, &path, force.unwrap_or(false))
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (pure parsing helpers; runnable on Linux).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_count_counts_nonblank_lines() {
        assert_eq!(parse_porcelain_count(""), 0);
        assert_eq!(parse_porcelain_count("\n"), 0);
        assert_eq!(parse_porcelain_count(" M src/a.rs\n"), 1);
        assert_eq!(
            parse_porcelain_count(" M src/a.rs\n?? new.txt\n D gone.rs\n"),
            3
        );
        // A trailing newline / blank line must not inflate the count.
        assert_eq!(parse_porcelain_count(" M a\n\n M b\n\n"), 2);
    }

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
}
