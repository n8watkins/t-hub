// Typed wrappers over the Git IPC surface (branch/worktree info + commit), for
// the Files panel's git awareness (feat/git-panel). Mirrors the Rust commands +
// `GitInfo` struct in `src-tauri/src/git.rs` (which serializes camelCase). Kept
// alongside ./files (same invoke pattern) so the git contract lives in one place.

import { invoke } from "@tauri-apps/api/core";

/** Git command names (used with `invoke`). Mirrors the lib.rs registration. */
export const CommandsGit = {
  /** Branch / worktree / dirty-count for a cwd. → GitInfo */
  gitInfo: "git_info",
  /** Stage all + commit with a message. → new short hash (or git output) */
  gitCommit: "git_commit",
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

/** Report git facts for `cwd`. Best-effort: a non-repo yields `isRepo: false`. */
export function gitInfo(cwd: string): Promise<GitInfo> {
  return invoke(CommandsGit.gitInfo, { cwd });
}

/**
 * Stage all changes (`git add -A`) and commit them with `message`. Resolves to
 * the new commit's short hash (or git's output); rejects on an empty message or
 * a git failure (e.g. nothing to commit).
 */
export function gitCommit(cwd: string, message: string): Promise<string> {
  return invoke(CommandsGit.gitCommit, { cwd, message });
}
