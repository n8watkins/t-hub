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
  /** List recent layout-snapshot history (Recovery review), newest first. */
  listSnapshots: "list_snapshots",
  /** Fetch one history snapshot's full layout JSON by id (Recovery review). */
  getSnapshot: "get_snapshot",
  /** Read the SHARED, all-variants layout (~/.config/t-hub/workspaces.json, #9). */
  loadSharedLayout: "load_shared_layout",
  /** Write the SHARED, all-variants layout. */
  saveSharedLayout: "save_shared_layout",
} as const;

/**
 * Lightweight metadata for one entry in the recovery snapshot history, mirroring
 * the Rust `SnapshotMeta` (`#[serde(rename_all = "camelCase")]`). The full layout
 * JSON is fetched separately via {@link getSnapshot} only when a row is previewed
 * or restored, so a list of these stays cheap.
 */
export interface SnapshotMeta {
  /** Stable row id; passed back to {@link getSnapshot}. */
  id: number;
  /** Unix epoch SECONDS the snapshot was captured. */
  ts: number;
  /** Human summary like `"5 tabs · 12 terminals"`, derived backend-side. */
  summary: string;
}

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

/**
 * Persist the layout to the SHARED, all-variants file (~/.config/t-hub/workspaces.json,
 * #9). Best-effort, like {@link saveWorkspaceSnapshot}: the SQLite + localStorage
 * copies remain authoritative; this is the cross-variant carrier (dev↔prod).
 */
export function saveSharedLayout(json: string): Promise<void> {
  return invoke(PersistenceCommands.saveSharedLayout, { layout: json });
}

/**
 * Load the SHARED, all-variants workspace layout JSON (#9), or `null` when none
 * exists yet / the backend is absent. Adopted on a fresh variant whose per-variant
 * durable copy is empty, so your workspaces follow you across a dev↔prod switch.
 */
export async function loadSharedLayout(): Promise<string | null> {
  const v = await invoke<string | null>(PersistenceCommands.loadSharedLayout);
  return v ?? null;
}

/**
 * List the recent workspace-layout snapshots (Recovery review, #recovery),
 * newest first. Returns lightweight metadata only; call {@link getSnapshot} for
 * the full JSON of a chosen entry. Resolves to `[]` when there's no history yet
 * or the backend is absent (the caller swallows that as "nothing to recover").
 */
export function listSnapshots(): Promise<SnapshotMeta[]> {
  return invoke<SnapshotMeta[]>(PersistenceCommands.listSnapshots);
}

/**
 * Fetch one history snapshot's full layout JSON by id, or `null` if it has aged
 * out of the ring / the backend is unavailable. The same v2-snapshot string the
 * boot hydration path parses — the Recovery UI runs it through that same parser.
 */
export async function getSnapshot(id: number): Promise<string | null> {
  const v = await invoke<string | null>(PersistenceCommands.getSnapshot, { id });
  return v ?? null;
}
