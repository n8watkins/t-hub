// Typed IPC wrappers for durable workspace persistence (#sqlite phase 1).
//
// Mirrors `src-tauri/src/db.rs`:
//   - command `save_workspace_snapshot(json: String)` — upsert the layout JSON
//     under the kv key `workspace.v2` (the durable copy of localStorage).
//   - command `load_workspace_snapshot() -> Option<String>` — read it back, or
//     null when nothing is stored yet / the DB couldn't be opened.
//
// The store imports these LAZILY (dynamic `import()`) so it carries no hard
// dependency on Tauri — importing the workspace store in a plain web/test
// context (no `@tauri-apps/api` backend) must never throw. Keep the command
// names here in lockstep with the Rust side.
import { invoke } from "@tauri-apps/api/core";

/** Exact Tauri command names for durable workspace persistence. */
export const PersistenceCommands = {
  /** Upsert the workspace layout JSON into SQLite (kv key `workspace.v2`). */
  saveWorkspaceSnapshot: "save_workspace_snapshot",
  /** Read the durable workspace layout JSON back (null if none/unavailable). */
  loadWorkspaceSnapshot: "load_workspace_snapshot",
} as const;

/**
 * Persist the workspace layout `json` to the SQLite durable copy. Best-effort:
 * the caller fires this without awaiting and swallows errors, since the
 * localStorage mirror remains the live source if the backend is absent.
 */
export function saveWorkspaceSnapshot(json: string): Promise<void> {
  return invoke(PersistenceCommands.saveWorkspaceSnapshot, { json });
}

/**
 * Load the durable workspace layout JSON from SQLite, or `null` when nothing is
 * stored yet (fresh install) or the DB couldn't be opened. The Rust command
 * returns `Option<String>`, which arrives as `string | null` over IPC.
 */
export async function loadWorkspaceSnapshot(): Promise<string | null> {
  const v = await invoke<string | null>(
    PersistenceCommands.loadWorkspaceSnapshot,
  );
  return v ?? null;
}
