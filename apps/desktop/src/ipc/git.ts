// Typed wrappers over the Git IPC surface (branch/worktree info + commit), for
// the Files panel's git awareness (feat/git-panel). Mirrors the Rust commands +
// `GitInfo` struct in `src-tauri/src/git.rs` (which serializes camelCase). Kept
// alongside ./files (same invoke pattern) so the git contract lives in one place.

import { invoke } from "@tauri-apps/api/core";

import { controlRequest } from "./controlClient";

/** Git command names (used with `invoke`). Mirrors the lib.rs registration. */
export const CommandsGit = {
  /** Branch / worktree / dirty-count for a cwd. → GitInfo */
  gitInfo: "git_info",
  /** Stage all + commit with a message. → new short hash (or git output) */
  gitCommit: "git_commit",
  /** List the worktrees of the repo containing a cwd. → WorktreeInfo[] (WS-4) */
  gitWorktreeList: "git_worktree_list",
  /** Create/check out a worktree at a path (optionally a branch). → git output */
  gitWorktreeAdd: "git_worktree_add",
  /** Remove the worktree at a path (optionally forced). → void */
  gitWorktreeRemove: "git_worktree_remove",
} as const;

/**
 * Git facts about a project cwd. Mirrors the Rust `GitInfo` struct. When `cwd`
 * is not inside a git repo, `isRepo` is false and the rest are empty/zero.
 */
export interface GitInfo {
  /** True when `cwd` is inside a git working tree. */
  isRepo: boolean;
  /** Current branch (e.g. `main`), or null on a detached HEAD / non-repo. */
  branch: string | null;
  /** Absolute path to this working tree's root, or null. */
  worktreeRoot: string | null;
  /** True when `cwd` is a *linked* worktree (`git worktree add`). */
  isLinkedWorktree: boolean;
  /** Changed-entry count (`git status --porcelain` line count). 0 = clean. */
  dirtyCount: number;
}

/**
 * Report git facts for `cwd`. Best-effort: a non-repo yields `isRepo: false`.
 *
 * Server-split M3 (overlay source over the wire): routed over the control socket
 * (`git_info` in control.rs) instead of the in-process Tauri command —
 * shape-identical, so it's a transport swap. The daemon reuses the same per-cwd
 * TTL cache (the freeze fix), so a thin client gets the REMOTE project's git state.
 * Only `gitInfo` moves over the socket; the mutating git commands below stay on
 * `invoke` (process-changing — gated off the control channel).
 */
/**
 * In-flight dedup (Option B focus de-storm): a window-focus refresh fires
 * `gitInfo(cwd)` from EVERY open tile at once, and tiles in the same repo
 * (worktrees, multiple tiles in one project) repeat the same cwd. Concurrent
 * calls for an identical cwd now share ONE `control_request` round-trip instead
 * of N. The entry is cleared the instant the request settles, so a later call
 * (e.g. the post-commit refresh, after `gitCommit` invalidated the daemon's
 * per-cwd cache) always re-fetches fresh — there is NO retained client cache and
 * therefore no added staleness.
 */
const inflightGitInfo = new Map<string, Promise<GitInfo>>();

export function gitInfo(cwd: string): Promise<GitInfo> {
  const existing = inflightGitInfo.get(cwd);
  if (existing) return existing;
  const p = (controlRequest(CommandsGit.gitInfo, { cwd }) as Promise<GitInfo>).finally(
    () => {
      // Only clear if THIS promise is still the current entry — a concurrent
      // gitCommit invalidation (or a newer call) may have already replaced it, and
      // we must not delete that newer in-flight entry.
      if (inflightGitInfo.get(cwd) === p) inflightGitInfo.delete(cwd);
    },
  );
  inflightGitInfo.set(cwd, p);
  return p;
}

/**
 * Stage all changes (`git add -A`) and commit them with `message`. Resolves to
 * the new commit's short hash (or git's output); rejects on an empty message or
 * a git failure (e.g. nothing to commit).
 */
export function gitCommit(cwd: string, message: string): Promise<string> {
  return (invoke(CommandsGit.gitCommit, { cwd, message }) as Promise<string>).finally(
    () => {
      // The dirty count just changed. Drop any in-flight gitInfo for this cwd so
      // the post-commit refresh starts a FRESH fetch (the daemon cache is also
      // invalidated server-side) instead of possibly reusing a pre-commit
      // in-flight result and showing a stale dirty count for one refresh cycle.
      inflightGitInfo.delete(cwd);
    },
  );
}

/**
 * One worktree of a repository (WS-4). Mirrors the Rust `WorktreeInfo` struct.
 * The main worktree is reported first with `isLinked: false`; every linked
 * worktree (`git worktree add`) has `isLinked: true`.
 */
export interface WorktreeInfo {
  /** Absolute working-tree path of this worktree (POSIX inside WSL). */
  path: string;
  /** Short branch name checked out here, or null (detached / bare). */
  branch: string | null;
  /** True for a linked worktree; false for the main one. */
  isLinked: boolean;
}

/**
 * List the worktrees attached to the repo containing `cwd`. Best-effort: a
 * non-repo (or unreadable dir) resolves to an empty list rather than rejecting.
 */
export function gitWorktreeList(cwd: string): Promise<WorktreeInfo[]> {
  return invoke(CommandsGit.gitWorktreeList, { cwd });
}

/**
 * Create (or check out into) a worktree at `path` for the repo containing `cwd`
 * (`git worktree add <path> [branch]`). With `branch`, checks that branch out;
 * without, git creates a new branch from the path's final component. Resolves to
 * git's output; rejects with a clear message if the branch is already checked out
 * in another worktree, or on any other git failure.
 */
export function gitWorktreeAdd(
  cwd: string,
  path: string,
  branch?: string,
): Promise<string> {
  return invoke(CommandsGit.gitWorktreeAdd, { cwd, path, branch });
}

/**
 * Remove the worktree at `path` from the repo containing `cwd`
 * (`git worktree remove [--force] <path>`). git refuses a worktree with
 * uncommitted changes unless `force` is true. Rejects with git's message on
 * failure.
 */
export function gitWorktreeRemove(
  cwd: string,
  path: string,
  force?: boolean,
): Promise<void> {
  return invoke(CommandsGit.gitWorktreeRemove, { cwd, path, force });
}
