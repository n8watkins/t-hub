// Typed wrapper over the "recent recallable Claude sessions" IPC surface
// (feat/projects-sidebar). The sidebar's Recent list calls this to enumerate
// past Claude Code sessions the user can RECALL (re-spawn a terminal in the
// session's cwd and `claude --resume <id>`). Kept separate from ./client (0.1
// nucleus) / ./client05 (0.5 surface) / ./files so the feature's contract lives
// in one place. Mirrors `src-tauri/src/recent.rs` (structs there serialize
// `rename_all = "camelCase"`); keep the two in lockstep.

import { controlRequest } from "./controlClient";

/**
 * One recallable past Claude session, read from the on-disk Claude transcripts
 * (`~/.claude/projects/<project>/<id>.jsonl`). Mirrors the Rust `RecentSession`.
 */
export interface RecentSession {
  /** Claude's session id (the transcript filename stem); the `--resume <id>`
   *  handle the recall path passes back. */
  id: string;
  /** The working directory the session ran in (a WSL-side path); recall spawns
   *  the new terminal here so `claude --resume` finds the right project. */
  cwd: string;
  /** A friendly label: Claude's own summary when known, else the cwd basename. */
  label: string;
  /** The session's most-recent message text (read from the transcript tail) — the
   *  Recent row's "what we were last doing" subtitle. Empty when none was found. */
  lastText: string;
  /** Unix epoch SECONDS of last activity (transcript mtime); the list sorts
   *  newest-first by this. */
  lastSeen: number;
}

/**
 * List recent recallable Claude sessions, newest first (the backend sorts +
 * caps). Best-effort: the backend returns an empty list rather than erroring
 * when the transcript catalog can't be read, so callers can render an empty
 * Recent section without special-casing failure.
 *
 * Server-split M3 (first overlay source over the wire): routed over the control
 * socket (`recent_sessions` in control.rs) instead of the in-process Tauri
 * command — shape-identical, so it's a transport swap. A thin client now gets the
 * REMOTE daemon's recent list; the wire M2 stretches to a remote host.
 */
export function recentSessions(): Promise<RecentSession[]> {
  return controlRequest("recent_sessions") as Promise<RecentSession[]>;
}
