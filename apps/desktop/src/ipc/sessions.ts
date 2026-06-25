// Typed IPC wrappers for native session-restore (WS-6).
//
// Mirrors `src-tauri/src/db.rs`:
//   - command `list_orphaned_sessions() -> OrphanedSession[]` — the boot-time
//     restore catalog: recorded tile→session bindings whose tmux session is GONE
//     (app/backend/host restarted) but whose transcript still EXISTS, so
//     `claude --resume <sessionId>` can bring them back. Recording the bindings
//     is automatic on the status-ingest path (no frontend command for it).
//
// Kept self-contained (its own command-name table + payload type) like
// `persistence.ts`. Keep the command names in lockstep with the Rust side.
import { invoke } from "@tauri-apps/api/core";

/** Exact Tauri command names for native session-restore (WS-6). */
export const SessionCommands = {
  /** List resumable orphaned sessions left by an app/backend/host restart. */
  listOrphanedSessions: "list_orphaned_sessions",
} as const;

/**
 * A resumable orphaned Claude session (WS-6), mirroring the Rust `OrphanedSession`
 * (`#[serde(rename_all = "camelCase")]`). A tile we recorded whose tmux session is
 * gone but whose transcript still exists, so `claude --resume <sessionId>` in `cwd`
 * picks it back up.
 */
export interface OrphanedSession {
  /** Claude's session id — the `--resume <id>` handle the Restore button passes. */
  sessionId: string;
  /** The directory to resume the session in (spawned as the new tile's cwd). */
  cwd: string;
  /** A friendly label (transcript summary/first-prompt, else cwd basename). */
  label: string;
  /** Unix epoch SECONDS the binding was last recorded (sorts newest-first). */
  lastSeen: number;
}

/**
 * List the resumable orphaned Claude sessions for the boot-time restore catalog,
 * newest first. Resolves to `[]` when none were recorded / the backend is absent
 * (the caller swallows that as "nothing to restore").
 */
export function listOrphanedSessions(): Promise<OrphanedSession[]> {
  return invoke<OrphanedSession[]>(SessionCommands.listOrphanedSessions);
}
