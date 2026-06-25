// Worktree-target resolver (WS-9b) — frontend half of "anchor resolution".
//
// Per docs/WORKTREE-WORKFLOW.md, a worktree is a sibling folder of the MAIN
// checkout: `<parent-of-repo>/<repo>-worktrees/<branch-sanitized>`. This module
// turns (a tile's LIVE cwd, a branch name) into that concrete target, anchored
// to the MAIN repo root — so "new worktree" triggered from INSIDE a linked
// worktree still lands as a sibling of the main checkout, never nested.
//
// The repo comes from where you already are (never guessed): we ask the shipped
// `gitWorktreeList` (WS-4) for the worktrees of the cwd's repo, and the MAIN
// root is the one entry with `isLinked === false` (git lists it first). An empty
// list (or a missing path) means the cwd isn't in a repo → `{ kind: "no-repo" }`,
// the signal the caller uses to pop the repo picker (WS-9d) instead.
//
// Everything here is PURE and side-effect-free except the single
// `gitWorktreeList` call. Paths are POSIX (we run inside WSL), so we do PURE
// string ops on "/" rather than node's `path` (which would behave per-platform).

import { gitWorktreeList } from "../ipc/git";

/**
 * The result of resolving a worktree target for a (cwd, branch) pair.
 *
 * - `ok`     — `cwd` is inside a git repo; `repoRoot` is the MAIN checkout root,
 *              `worktreePath` is the sibling folder to create, and `branch` is
 *              the ORIGINAL git branch name (slashes intact — only the directory
 *              component is sanitized).
 * - `no-repo`— `cwd` is not inside a git repo (e.g. a `~` scratch shell); the
 *              caller should ask (repo picker), not guess a path.
 */
export type WorktreeTarget =
  | { kind: "ok"; repoRoot: string; worktreePath: string; branch: string }
  | { kind: "no-repo" };

/**
 * POSIX `dirname`: the parent directory of `p`, without a trailing slash.
 * Pure string op on "/" (no node `path`). A trailing slash on `p` is ignored
 * (`/a/b/` → `/a`). A root-level path (`/foo`) yields `""`; `/` yields `""`.
 */
export function posixDirname(p: string): string {
  // Drop any trailing slashes so `/a/b/` is treated like `/a/b`.
  const trimmed = p.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx < 0) return "";
  // Keep the leading "/" for an absolute root child (`/foo` → "" not garbage,
  // but `/a/b` → `/a`). lastIndexOf at 0 means the parent is the filesystem root.
  return trimmed.slice(0, idx);
}

/**
 * POSIX `basename`: the final path component of `p`, without surrounding slashes.
 * Pure string op on "/" (no node `path`). A trailing slash is ignored
 * (`/a/b/` → `b`); a bare `/` (or empty) yields `""`.
 */
export function posixBasename(p: string): string {
  const trimmed = p.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  return idx < 0 ? trimmed : trimmed.slice(idx + 1);
}

/**
 * Turn a git branch name into a SAFE directory component for the sibling
 * worktree folder. The ORIGINAL branch keeps its slashes (it's a real git
 * branch); only the on-disk directory is flattened.
 *
 * Rules: trim whitespace; strip leading/trailing "/"; replace every char NOT in
 * `[A-Za-z0-9._-]` (slashes included) with "-"; collapse runs of "-" into one;
 * trim stray leading/trailing "-"; fall back to `"work"` if nothing remains.
 *
 * e.g. `feature/x` → `feature-x`, `  /a//b/  ` → `a-b`, `!!!` → `work`.
 */
export function sanitizeBranchToDir(branch: string): string {
  const dir = branch
    .trim()
    .replace(/^\/+|\/+$/g, "") // strip leading/trailing slashes
    .replace(/[^A-Za-z0-9._-]+/g, "-") // any disallowed char (incl. "/") → "-"
    .replace(/-+/g, "-") // collapse repeated "-"
    .replace(/^-+|-+$/g, ""); // trim stray "-" at the ends
  return dir.length > 0 ? dir : "work";
}

/**
 * Resolve a (cwd, branch) pair to a concrete worktree target anchored to the
 * MAIN repo root (WS-9b). See {@link WorktreeTarget}.
 *
 * The MAIN root is the `gitWorktreeList` entry with `isLinked === false` (git
 * lists it first; we fall back to the first entry defensively). The sibling
 * path is `<parent-of-root>/<root-name>-worktrees/<sanitized-branch>`. The
 * returned `branch` is the UNMODIFIED input (slashes intact).
 *
 * Best-effort, like `gitWorktreeList`: an empty list — or any entry without a
 * usable path — resolves to `{ kind: "no-repo" }` rather than rejecting.
 */
export async function resolveWorktreeTarget(
  cwd: string,
  branch: string,
): Promise<WorktreeTarget> {
  const list = await gitWorktreeList(cwd);

  // MAIN repo root = the non-linked entry (git lists it first); fall back to the
  // first entry if every flag somehow says linked. No usable path → not a repo.
  const repoRoot = list.find((w) => !w.isLinked)?.path ?? list[0]?.path;
  if (!repoRoot) return { kind: "no-repo" };

  const parent = posixDirname(repoRoot);
  const name = posixBasename(repoRoot);
  const dir = sanitizeBranchToDir(branch);
  const worktreePath = `${parent}/${name}-worktrees/${dir}`;

  return { kind: "ok", repoRoot, worktreePath, branch };
}
