// Repo-picker candidate cwds (WS-9d) — the data behind the "Pick a repo…"
// fallback the WorktreePrompt shows when there's no repo in the focused tile's
// context (a `~` scratch shell, or an empty workspace).
//
// Per docs/WORKTREE-WORKFLOW.md the picker list is built from data we ALREADY
// have — no new IPC, no per-candidate git probe:
//   - the LIVE cwds of every open tile (`useWorkspace.getState().terminals`), and
//   - the cwds of recent recallable Claude sessions (`recentSessions()`).
//
// These are ANCHOR cwds, not repo roots: we deliberately DON'T git-resolve each
// (that'd be N git calls for a list the user may never open). The repo root is
// resolved lazily by `resolveWorktreeTarget(pickedCwd, branch)` when the user
// actually picks one — the same cheap one-call path the normal flow uses.
//
// `dedupeCwds` is split out as a PURE helper (no store/IPC import) so it's
// trivially testable and reusable: it dedupes while PRESERVING the first
// occurrence's order, so the caller controls "most relevant first" by ordering
// its inputs (open tiles before recent sessions).

import { useWorkspace } from "../store/workspace";
import { recentSessions } from "../ipc/recent";

/**
 * Dedupe a list of cwds, keeping the FIRST occurrence of each (order-preserving),
 * dropping empties/whitespace and ignoring a trailing slash when comparing
 * (`/a/b` and `/a/b/` are the same dir). The returned strings are the trimmed,
 * trailing-slash-stripped form. Pure — no side effects, no IPC.
 */
export function dedupeCwds(cwds: Iterable<string>): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of cwds) {
    const cwd = (raw ?? "").trim().replace(/\/+$/, "");
    if (!cwd) continue;
    if (seen.has(cwd)) continue;
    seen.add(cwd);
    out.push(cwd);
  }
  return out;
}

/**
 * The distinct cwds to offer as repo-picker anchors (WS-9d), most-relevant first.
 *
 * Ordering: open tiles' live cwds FIRST (where the user is right now), then
 * recent-session cwds — `dedupeCwds` keeps each cwd's first occurrence, so an
 * open tile's cwd wins over the same path appearing later in Recent.
 *
 * Cheap by design: no per-candidate git resolution (the repo root is resolved on
 * selection by `resolveWorktreeTarget`). Best-effort — if `recentSessions()`
 * rejects we still return the open-tile cwds rather than failing the picker.
 */
export async function candidateRepoCwds(): Promise<string[]> {
  const tileCwds = Object.values(useWorkspace.getState().terminals)
    .map((t) => t.cwd ?? "")
    .filter(Boolean);

  let sessionCwds: string[] = [];
  try {
    sessionCwds = (await recentSessions()).map((s) => s.cwd);
  } catch {
    // Best-effort: a failed Recent read just means fewer candidates, not no picker.
    sessionCwds = [];
  }

  return dedupeCwds([...tileCwds, ...sessionCwds]);
}
