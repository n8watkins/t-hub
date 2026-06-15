// Per-tile context-window meter feed (feat/context-meter).
//
// WHY a separate store: each terminal tile runs `wsl → tmux → claude`, and we
// want to show THAT session's context-window fullness on the tile. The data
// already reaches the frontend — Claude's statusline JSON carries
// `context_window`, which the agent journals and the core re-emits as the
// `status://snapshot` event (see src/claude/status.rs + agent/mod.rs). The
// catch is the KEY: snapshots are keyed by Claude `session_id`, and the
// frontend has NO reliable terminal-id→session-id bridge (the only correlation
// anywhere is a best-effort cwd match — see workspace.ts `terminalForCwd`).
//
// The fix that unlocks a tile↔session link is tiny: the snapshot now also
// carries the session's `cwd` (added to StatusSnapshot), so we can index context
// usage by NORMALIZED cwd and a tile can look itself up by its own `info.cwd`.
// This is the task's accepted FIRST CUT (match by cwd), and it degrades to
// nothing when no Claude session shares the tile's directory.
//
// It deliberately keeps its OWN `status://snapshot` subscription (Tauri fans an
// event out to every listener) rather than reshaping the supervision store,
// which keys by session id and carries no cwd — staying out of that store keeps
// this feature self-contained and cleanly revertible.
import { create } from "zustand";
import { onStatus, type StatusSnapshotWire } from "../ipc/client05";

/** Normalize a cwd for correlation: drop trailing separators and lower-case it
 *  (WSL paths are case-insensitive in practice). Mirrors workspace.ts `normCwd`
 *  so a tile's cwd and a snapshot's cwd compare the same way. Empty → "". */
export function normCwd(cwd: string | undefined | null): string {
  if (!cwd) return "";
  return cwd.replace(/[/\\]+$/, "").toLowerCase();
}

/** One session's context-window reading, keyed in the store by normalized cwd.
 *  `usedPct` is the 0..=100 fullness; `ts` is the snapshot's ingest time so a
 *  newer reading always wins even if events arrive out of order. */
interface CtxReading {
  usedPct: number;
  ts: number;
}

interface SessionContextState {
  /** context-window fullness per NORMALIZED cwd (see normCwd). */
  byCwd: Record<string, CtxReading>;
  /** Fold a status snapshot in (no-op unless it has both a cwd and a context %). */
  ingest: (snap: StatusSnapshotWire) => void;
}

export const useSessionContext = create<SessionContextState>((set) => ({
  byCwd: {},
  ingest: (snap) =>
    set((s) => {
      const key = normCwd(snap.cwd);
      // Need BOTH a directory to key on and an actual context % to show.
      if (!key || snap.contextUsedPct == null) return s;
      const prev = s.byCwd[key];
      // Keep the freshest reading; ignore an older/equal-timestamp duplicate.
      if (prev && prev.ts >= snap.ingestedAtMs) return s;
      return {
        byCwd: {
          ...s.byCwd,
          [key]: { usedPct: snap.contextUsedPct, ts: snap.ingestedAtMs },
        },
      };
    }),
}));

/** Look up a tile's context-window fullness (0..=100) by its cwd, or null when
 *  no Claude session is matched to that directory. Pure selector for the meter
 *  — a tile calls this with its own `info.cwd`. Starts the singleton snapshot
 *  feed on first use (idempotent), so the meter works without a separate
 *  always-mounted subscriber component. */
export function useContextPctForCwd(cwd: string | undefined): number | null {
  ensureFeed();
  return useSessionContext((s) => {
    const r = s.byCwd[normCwd(cwd)];
    return r ? r.usedPct : null;
  });
}

/**
 * Start the `status://snapshot` → store feed exactly ONCE for the app lifetime
 * (a module singleton, not per-component), the first time any tile reads the
 * store. This keeps the feature self-contained — it needs no edit to a parent
 * component to wire a subscription — while still subscribing only once. The
 * listener lives for the whole session (never torn down): snapshots are cheap
 * and the cockpit is a long-lived window. Degrades silently when the Tauri event
 * runtime is absent (dev/web) — the meter simply stays empty.
 */
let feedStarted = false;
function ensureFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  void onStatus((snap) => {
    useSessionContext.getState().ingest(snap);
  }).catch(() => {
    // No Tauri event runtime — allow a later retry rather than wedging the flag.
    feedStarted = false;
  });
}
