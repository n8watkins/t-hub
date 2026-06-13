//! Host facts: WSL metrics (RAM/swap/CPU/load/process count/distro) and
//! git/worktree queries (PLAN.md Workstream A + §H).
//!
//! These run inside WSL, reading `/proc` and shelling out to `git`. The
//! statusline does NOT carry the non-worktree branch, so `git_branch` is how the
//! core derives it (REVIEW / PLAN §10.4).
//!
//! SUBAGENT(host): implement the real `/proc/meminfo`, `/proc/loadavg`,
//! `/proc/cpuinfo`/`nproc`, `/proc` process count, `/etc/os-release` parsing,
//! and the two git commands. The function signatures + return types are the
//! contract (they feed `termhub_protocol::HostMetrics` / `WorktreeInfo`) and
//! must not change. The stubs below compile and return safe defaults so the
//! agent runs end-to-end before this is filled in.

use anyhow::Result;
use termhub_protocol::{HostMetrics, WorktreeInfo};

/// Current epoch-millis on the agent clock.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Collect a one-shot host metrics snapshot.
///
/// SUBAGENT(host): populate every field from `/proc` + `/etc/os-release`.
/// Currently returns a default snapshot with only `captured_at_ms` set so the
/// protocol path is exercisable.
pub fn metrics() -> HostMetrics {
    HostMetrics {
        captured_at_ms: now_ms(),
        ..Default::default()
    }
}

/// `git branch --show-current` in `cwd`. `Ok(None)` when detached / not a repo.
///
/// SUBAGENT(host): run the command, mapping a non-zero exit (not a repo /
/// detached) to `Ok(None)` and an io failure to `Err`.
pub fn git_branch(_cwd: &str) -> Result<Option<String>> {
    Ok(None)
}

/// `git worktree list --porcelain` for the repo containing `cwd`.
///
/// SUBAGENT(host): run the command and parse the porcelain records into
/// `WorktreeInfo` (`worktree`/`branch`/`HEAD`/`bare`/`detached` stanzas). Return
/// an empty Vec when `cwd` isn't in a repo.
pub fn git_worktrees(_cwd: &str) -> Result<Vec<WorktreeInfo>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_stub_sets_timestamp() {
        let m = metrics();
        assert!(m.captured_at_ms > 0);
    }
}
