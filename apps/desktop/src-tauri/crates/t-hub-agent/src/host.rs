//! Host facts: WSL metrics (RAM/swap/CPU/load/process count/distro) and
//! git/worktree queries (PLAN.md Workstream A + §H).
//!
//! These run inside WSL, reading `/proc` and shelling out to `git`. The
//! statusline does NOT carry the non-worktree branch, so `git_branch` is how the
//! core derives it (REVIEW / PLAN §10.4).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use t_hub_protocol::{GitInfo, HostMetrics, WorktreeInfo};

/// The desktop bridge abandons any RPC after 10 seconds. Keep this collector
/// strictly shorter so the agent's single request loop is free before desktop
/// fallback begins instead of continuing stale work in the background.
const GIT_INFO_TIMEOUT_MAX: Duration = Duration::from_secs(9);

const GIT_INFO_SCRIPT: &str = "\
inside=$(git rev-parse --is-inside-work-tree 2>/dev/null); \
printf 'inside\\t%s\\n' \"$inside\"; \
if [ \"$inside\" = true ]; then \
printf 'branch\\t%s\\n' \"$(git rev-parse --abbrev-ref HEAD 2>/dev/null)\"; \
printf 'toplevel\\t%s\\n' \"$(git rev-parse --show-toplevel 2>/dev/null)\"; \
printf 'gitdir\\t%s\\n' \"$(git rev-parse --git-dir 2>/dev/null)\"; \
printf 'commondir\\t%s\\n' \"$(git rev-parse --git-common-dir 2>/dev/null)\"; \
printf 'dirty\\t%s\\n' \"$(git status --porcelain 2>/dev/null | wc -l)\"; \
printf 'head\\t%s\\n' \"$(git rev-parse HEAD 2>/dev/null)\"; \
printf 'remote\\t%s\\n' \"$(git remote get-url origin 2>/dev/null)\"; \
printf 'default\\t%s\\n' \"$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's#^origin/##')\"; \
fi";

fn git_info_timeout() -> Duration {
    let configured = std::env::var("T_HUB_GIT_CMD_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(GIT_INFO_TIMEOUT_MAX);
    bound_git_info_timeout(configured)
}

fn bound_git_info_timeout(configured: Duration) -> Duration {
    configured.min(GIT_INFO_TIMEOUT_MAX)
}

/// Current epoch-millis on the agent clock.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Collect a one-shot host metrics snapshot by reading Linux `/proc` files
/// and `/etc/os-release`. Every field degrades gracefully on missing/malformed
/// input (0 / empty / None) — this function never panics.
pub fn metrics() -> HostMetrics {
    // --- /proc/meminfo ---
    let mut mem_total_kib: u64 = 0;
    let mut mem_available_kib: u64 = 0;
    let mut swap_total_kib: u64 = 0;
    let mut swap_free_kib: u64 = 0;

    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            let mut parts = line.splitn(2, ':');
            let key = parts.next().unwrap_or("").trim();
            let val_str = parts.next().unwrap_or("").trim();
            // Values look like "12345678 kB" — take the first token.
            let val: u64 = val_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match key {
                "MemTotal" => mem_total_kib = val,
                "MemAvailable" => mem_available_kib = val,
                "SwapTotal" => swap_total_kib = val,
                "SwapFree" => swap_free_kib = val,
                _ => {}
            }
        }
    }

    // --- /proc/cpuinfo — count "processor" lines ---
    let cpu_count: u32 = if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        content
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count() as u32
    } else {
        // Fallback: ask the runtime.
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1)
    };

    // --- /proc/loadavg — first three whitespace-separated floats ---
    let load_avg: [f32; 3] = if let Ok(content) = std::fs::read_to_string("/proc/loadavg") {
        let mut it = content.split_whitespace();
        let a = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let b = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let c = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        [a, b, c]
    } else {
        [0.0, 0.0, 0.0]
    };

    // --- /proc — count all-numeric directory names (PIDs) ---
    let process_count: u32 = std::fs::read_dir("/proc")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .chars()
                        .all(|c| c.is_ascii_digit())
                })
                .count() as u32
        })
        .unwrap_or(0);

    // --- /etc/os-release — PRETTY_NAME, strip surrounding double-quotes ---
    let distro: Option<String> = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("PRETTY_NAME=")).map(|l| {
                let val = l["PRETTY_NAME=".len()..].trim();
                val.trim_matches('"').to_string()
            })
        });

    HostMetrics {
        mem_total_kib,
        mem_available_kib,
        swap_total_kib,
        swap_free_kib,
        cpu_count,
        load_avg,
        process_count,
        distro,
        captured_at_ms: now_ms(),
    }
}

/// Collect the complete Files-panel git snapshot in one bounded shell process.
pub fn git_info(cwd: &str) -> Result<GitInfo> {
    let mut child = Command::new("bash")
        .current_dir(cwd)
        .args(["-lc", GIT_INFO_SCRIPT])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn git info script: {e}"))?;
    let deadline = Instant::now() + git_info_timeout();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow::anyhow!("git info script timed out"));
            }
            Err(e) => return Err(anyhow::anyhow!("failed waiting for git info script: {e}")),
        }
    }

    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)
            .map_err(|e| anyhow::anyhow!("failed reading git info output: {e}"))?;
    }
    Ok(parse_git_info_output(&stdout))
}

fn parse_git_info_output(stdout: &str) -> GitInfo {
    let fields: HashMap<&str, &str> = stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    if fields.get("inside").map(|value| value.trim()) != Some("true") {
        return GitInfo::default();
    }

    let first_line = |key: &str| {
        fields.get(key).and_then(|value| {
            let value = value.lines().next().unwrap_or("").trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    };
    let git_dir = first_line("gitdir");
    let common_dir = first_line("commondir");
    GitInfo {
        is_repo: true,
        branch: first_line("branch").filter(|branch| branch != "HEAD"),
        worktree_root: first_line("toplevel"),
        is_linked_worktree: matches!(
            (git_dir.as_deref(), common_dir.as_deref()),
            (Some(git_dir), Some(common_dir)) if git_dir.trim() != common_dir.trim()
        ),
        dirty_count: fields
            .get("dirty")
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0),
        head_commit: first_line("head"),
        remote_url: first_line("remote"),
        default_branch: first_line("default"),
    }
}

/// `git branch --show-current` in `cwd`.
///
/// Returns:
/// - `Ok(Some(branch))` when on a named branch.
/// - `Ok(None)` when in a detached HEAD state (git prints an empty line) or
///   when `cwd` is not inside a git repository (non-zero exit).
/// - `Err` only on I/O failure spawning the git process.
pub fn git_branch(cwd: &str) -> Result<Option<String>> {
    let output = std::process::Command::new("git")
        .args(["-C", cwd, "branch", "--show-current"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn git: {e}"))?;

    if !output.status.success() {
        // Non-zero exit: cwd isn't a git repo (or some other git error).
        return Ok(None);
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        Ok(None) // Detached HEAD.
    } else {
        Ok(Some(branch))
    }
}

/// `git worktree list --porcelain` for the repo containing `cwd`.
///
/// Parses the porcelain output into one [`WorktreeInfo`] per stanza (records
/// separated by blank lines). Returns an empty `Vec` when `cwd` is not inside
/// a git repository (non-zero exit). Only an I/O spawn failure returns `Err`.
pub fn git_worktrees(cwd: &str) -> Result<Vec<WorktreeInfo>> {
    let output = std::process::Command::new("git")
        .args(["-C", cwd, "worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn git: {e}"))?;

    if !output.status.success() {
        // Not a git repo.
        return Ok(Vec::new());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();

    // Porcelain format: stanzas separated by blank lines.
    // Each stanza starts with "worktree <path>", then optional key lines.
    let mut current: Option<WorktreeInfo> = None;

    for line in text.lines() {
        if line.is_empty() {
            // End of a stanza — flush.
            if let Some(wt) = current.take() {
                worktrees.push(wt);
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            // Start of a new stanza (flush the previous one just in case there
            // was no trailing blank line before this one).
            if let Some(wt) = current.take() {
                worktrees.push(wt);
            }
            current = Some(WorktreeInfo {
                path: path.to_string(),
                branch: None,
                head: None,
                bare: false,
                detached: false,
            });
            continue;
        }

        if let Some(wt) = current.as_mut() {
            if let Some(sha) = line.strip_prefix("HEAD ") {
                wt.head = Some(sha.to_string());
            } else if let Some(branch_ref) = line.strip_prefix("branch ") {
                // Strip "refs/heads/" prefix.
                let branch = branch_ref.strip_prefix("refs/heads/").unwrap_or(branch_ref);
                wt.branch = Some(branch.to_string());
            } else if line == "bare" {
                wt.bare = true;
            } else if line == "detached" {
                wt.detached = true;
            }
            // Any other key lines (e.g. "locked", "prunable") are ignored.
        }
    }

    // Flush a final stanza that had no trailing blank line.
    if let Some(wt) = current.take() {
        worktrees.push(wt);
    }

    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_repo(tag: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("t-hub-agent-{tag}-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "T-Hub Test"],
            vec!["config", "user.email", "t-hub@example.test"],
        ] {
            assert!(Command::new("git")
                .current_dir(&path)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(path.join("tracked.txt"), "initial\n").unwrap();
        assert!(Command::new("git")
            .current_dir(&path)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(&path)
            .args(["commit", "-m", "initial"])
            .status()
            .unwrap()
            .success());
        path
    }

    #[test]
    fn metrics_timestamp_is_set() {
        let m = metrics();
        assert!(m.captured_at_ms > 0, "captured_at_ms must be non-zero");
    }

    #[test]
    fn metrics_mem_total_is_positive() {
        let m = metrics();
        assert!(
            m.mem_total_kib > 0,
            "mem_total_kib should be > 0 on this Linux host"
        );
    }

    #[test]
    fn metrics_cpu_count_at_least_one() {
        let m = metrics();
        assert!(m.cpu_count >= 1, "cpu_count must be >= 1");
    }

    #[test]
    fn metrics_distro_is_ubuntu() {
        let m = metrics();
        let distro = m.distro.expect("distro should be Some on Ubuntu");
        assert!(
            distro.contains("Ubuntu"),
            "distro should contain 'Ubuntu', got: {distro}"
        );
    }

    #[test]
    fn git_branch_in_t_hub_worktree() {
        // Use THIS crate's own directory (a real git checkout on any machine) rather
        // than a hardcoded absolute path to one dev's clone, which does not exist in
        // CI / other checkouts and made this test spuriously fail.
        let repo = env!("CARGO_MANIFEST_DIR");
        let branch = git_branch(repo).expect("git_branch should not error in a valid git worktree");
        // NIT-2b: tolerate a DETACHED HEAD (git worktree add --detach, some CI checkout
        // modes) where `git branch --show-current` is empty -> None. When Some, it must
        // be non-empty.
        if let Some(branch) = branch {
            assert!(
                !branch.is_empty(),
                "branch string should be non-empty when on a branch, got: {branch}"
            );
        }
    }

    #[test]
    fn git_branch_not_a_repo_returns_none() {
        let result = git_branch("/tmp").expect("git_branch on /tmp should return Ok, not Err");
        assert!(
            result.is_none(),
            "git_branch on /tmp should return None (not a git repo)"
        );
    }

    #[test]
    fn git_worktrees_in_t_hub_repo() {
        // Portable: query THIS crate's own git repo (see git_branch_in_t_hub_worktree),
        // not a machine-specific absolute path.
        let repo = env!("CARGO_MANIFEST_DIR");
        let worktrees =
            git_worktrees(repo).expect("git_worktrees should not error in a valid git repo");
        assert!(
            !worktrees.is_empty(),
            "should find at least one worktree entry"
        );
        // Every entry must have a non-empty path.
        for wt in &worktrees {
            assert!(!wt.path.is_empty(), "worktree path should not be empty");
        }
    }

    #[test]
    fn git_worktrees_not_a_repo_returns_empty() {
        let worktrees =
            git_worktrees("/tmp").expect("git_worktrees on /tmp should return Ok, not Err");
        assert!(
            worktrees.is_empty(),
            "git_worktrees on /tmp should return empty Vec"
        );
    }

    #[test]
    fn git_info_reports_real_repo_and_linked_worktree_facts() {
        let repo = scratch_repo("git-info");
        std::fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
        assert!(Command::new("git")
            .current_dir(&repo)
            .args(["remote", "add", "origin", "https://example.test/repo.git"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(&repo)
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main"
            ])
            .status()
            .unwrap()
            .success());

        let main = git_info(repo.to_str().unwrap()).unwrap();
        assert!(main.is_repo);
        assert_eq!(main.branch.as_deref(), Some("main"));
        assert_eq!(main.worktree_root.as_deref(), repo.to_str());
        assert!(!main.is_linked_worktree);
        assert_eq!(main.dirty_count, 1);
        assert!(main.head_commit.is_some());
        assert_eq!(
            main.remote_url.as_deref(),
            Some("https://example.test/repo.git")
        );
        assert_eq!(main.default_branch.as_deref(), Some("main"));

        let linked = repo.with_extension("linked");
        assert!(Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "add", "-b", "linked", linked.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let linked_info = git_info(linked.to_str().unwrap()).unwrap();
        assert!(linked_info.is_linked_worktree);
        assert_eq!(linked_info.branch.as_deref(), Some("linked"));

        std::fs::remove_dir_all(&linked).ok();
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn git_info_non_repo_is_empty() {
        let dir = std::env::temp_dir().join(format!("t-hub-agent-nonrepo-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(git_info(dir.to_str().unwrap()).unwrap(), GitInfo::default());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn git_info_timeout_finishes_before_bridge_request_timeout() {
        assert_eq!(
            bound_git_info_timeout(Duration::from_secs(45)),
            Duration::from_secs(9)
        );
        assert_eq!(
            bound_git_info_timeout(Duration::from_secs(4)),
            Duration::from_secs(4)
        );
        assert!(GIT_INFO_TIMEOUT_MAX < Duration::from_secs(10));
    }
}
