//! Host facts: WSL metrics (RAM/swap/CPU/load/process count/distro) and
//! git/worktree queries (PLAN.md Workstream A + §H).
//!
//! These run inside WSL, reading `/proc` and shelling out to `git`. The
//! statusline does NOT carry the non-worktree branch, so `git_branch` is how the
//! core derives it (REVIEW / PLAN §10.4).

use anyhow::Result;
use termhub_protocol::{HostMetrics, WorktreeInfo};

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
            let val: u64 = val_str.split_whitespace().next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match key {
                "MemTotal"    => mem_total_kib     = val,
                "MemAvailable"=> mem_available_kib  = val,
                "SwapTotal"   => swap_total_kib     = val,
                "SwapFree"    => swap_free_kib      = val,
                _ => {}
            }
        }
    }

    // --- /proc/cpuinfo — count "processor" lines ---
    let cpu_count: u32 = if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        content.lines()
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
    let distro: Option<String> = std::fs::read_to_string("/etc/os-release").ok().and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("PRETTY_NAME="))
            .map(|l| {
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
                let branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch_ref);
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

    #[test]
    fn metrics_timestamp_is_set() {
        let m = metrics();
        assert!(m.captured_at_ms > 0, "captured_at_ms must be non-zero");
    }

    #[test]
    fn metrics_mem_total_is_positive() {
        let m = metrics();
        assert!(m.mem_total_kib > 0, "mem_total_kib should be > 0 on this Linux host");
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
    fn git_branch_in_termhub_worktree() {
        let branch = git_branch("/home/natkins/n8builds/termhub-05")
            .expect("git_branch should not error in a valid git worktree");
        let branch = branch.expect("branch should be Some in feat/0.5-personal-alpha worktree");
        assert!(
            !branch.is_empty(),
            "branch string should be non-empty, got: {branch}"
        );
        assert_eq!(
            branch, "feat/0.5-personal-alpha",
            "expected branch feat/0.5-personal-alpha"
        );
    }

    #[test]
    fn git_branch_not_a_repo_returns_none() {
        let result = git_branch("/tmp")
            .expect("git_branch on /tmp should return Ok, not Err");
        assert!(
            result.is_none(),
            "git_branch on /tmp should return None (not a git repo)"
        );
    }

    #[test]
    fn git_worktrees_in_termhub_repo() {
        let worktrees = git_worktrees("/home/natkins/n8builds/termhub-05")
            .expect("git_worktrees should not error in a valid git repo");
        assert!(
            !worktrees.is_empty(),
            "should find at least one worktree entry"
        );
        // The worktree-05 entry should appear somewhere in the list with the expected branch.
        let found = worktrees.iter().any(|wt| {
            wt.branch.as_deref() == Some("feat/0.5-personal-alpha")
        });
        assert!(found, "expected to find the feat/0.5-personal-alpha worktree in list");
        // Every entry must have a non-empty path.
        for wt in &worktrees {
            assert!(!wt.path.is_empty(), "worktree path should not be empty");
        }
    }

    #[test]
    fn git_worktrees_not_a_repo_returns_empty() {
        let worktrees = git_worktrees("/tmp")
            .expect("git_worktrees on /tmp should return Ok, not Err");
        assert!(
            worktrees.is_empty(),
            "git_worktrees on /tmp should return empty Vec"
        );
    }
}
