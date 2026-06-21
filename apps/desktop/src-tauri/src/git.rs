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
}
