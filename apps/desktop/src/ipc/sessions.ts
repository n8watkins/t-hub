// Typed IPC wrappers for native session-restore (WS-6).
//
// Mirrors `src-tauri/src/db.rs`:
//   - command `record_tile_session(terminalId, sessionId, cwd, tmuxSession)` —
//     upsert the Claude session a tile is hosting (the status-ingest path records
//     this automatically; the command exists for a future native hook).
//   - command `list_orphaned_sessions() -> OrphanedSession[]` — the boot-time
//     restore catalog: recorded tile→session bindings whose tmux session is GONE
//     (app/backend/host restarted) but whose transcript still EXISTS, so
//     `claude --resume <sessionId>` can bring them back.
//
// Kept self-contained (its own command-name table + payload type) like
// `persistence.ts`. Keep the command names in lockstep with the Rust side.
import { invoke } from "@tauri-apps/api/core";

/** Exact Tauri command names for native session-restore (WS-6). */
export const SessionCommands = {
  /** Upsert the Claude session a tile is hosting (per-tile session map). */
  recordTileSession: "record_tile_session",
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
 * Record (upsert) the Claude session a tile is hosting. Best-effort: the
 * status-ingest path already records this as statusline snapshots arrive, so a
 * UI caller can fire-and-forget. Keyed by `terminalId`.
 */
export function recordTileSession(
  terminalId: string,
  sessionId: string,
  cwd: string,
  tmuxSession: string,
): Promise<void> {
  return invoke(SessionCommands.recordTileSession, {
    terminalId,
    sessionId,
    cwd,
    tmuxSession,
  });
}

/**
 * List the resumable orphaned Claude sessions for the boot-time restore catalog,
 * newest first. Resolves to `[]` when none were recorded / the backend is absent
 * (the caller swallows that as "nothing to restore").
 */
export function listOrphanedSessions(): Promise<OrphanedSession[]> {
  return invoke<OrphanedSession[]>(SessionCommands.listOrphanedSessions);
}
