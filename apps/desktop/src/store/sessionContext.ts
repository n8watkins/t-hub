// Per-tile context-window meter feed (feat/context-meter).
//
// WHY a separate store: each terminal tile runs `wsl → tmux → claude`, and we
// want to show THAT session's context-window fullness on the tile. The data
// already reaches the frontend — Claude's statusline JSON carries
// `context_window`, which the agent journals and the core re-emits as the
// `status://snapshot` event (see src/claude/status.rs + agent/mod.rs). The
// catch was the KEY: snapshots are keyed by Claude `session_id`, and the
// frontend has NO terminal-id→session-id bridge.
//
// ROBUST binding (this store): the agent now stamps the OWNING TMUX SESSION onto
// each statusline before journaling — it reads `$TMUX_PANE` (the pane the
// statusline runs in) and resolves `#{session_name}` on the `t-hub` socket.
// T-Hub names every session `th_<terminalId>`, so the frontend keys context by
// that session name and a tile looks itself up by its OWN `th_<id>` (which it can
// compute directly). This is precise even when two tiles share a directory — the
// old cwd match could not tell them apart.
//
// FALLBACK (graceful degradation): if a snapshot carries no `tmuxSession` (an
// older agent that doesn't stamp it yet, or a session not under tmux), we still
// index it by NORMALIZED cwd and a tile can match on its `info.cwd` — the prior
// behavior. So the meter keeps working before the rebuilt agent is installed; it
// just gets more precise once it is.
//
// It deliberately keeps its OWN `status://snapshot` subscription (Tauri fans an
// event out to every listener) rather than reshaping the supervision store,
// which keys by session id and carries none of this — staying out of that store
// keeps this feature self-contained and cleanly revertible.
import { create } from "zustand";
import { onStatus, type StatusSnapshotWire } from "../ipc/client05";

/** Normalize a cwd for correlation: drop trailing separators and lower-case it
 *  (WSL paths are case-insensitive in practice). Mirrors workspace.ts `normCwd`
 *  so a tile's cwd and a snapshot's cwd compare the same way. Empty → "". */
export function normCwd(cwd: string | undefined | null): string {
  if (!cwd) return "";
  return cwd.replace(/[/\\]+$/, "").toLowerCase();
}

/** The tmux session name T-Hub gives a terminal: `th_<terminalId>`. This is
 *  the robust binding key — a tile computes it from its own id and looks up the
 *  context reading the agent reported for that exact session. Mirrors the
 *  backend (`tmux.rs` / `pty.rs` name every session `th_<id>`). */
export function sessionNameForTerminal(terminalId: string): string {
  return `th_${terminalId}`;
}

/** One session's context-window reading. `usedPct` is the 0..=100 fullness; `ts`
 *  is the snapshot's ingest time so a newer reading always wins even if events
 *  arrive out of order. */
interface CtxReading {
  usedPct: number;
  ts: number;
}

interface SessionContextState {
  /** context-window fullness per TMUX SESSION NAME (`th_<id>`) — the robust key
   *  the agent reports. Checked FIRST by a tile (it knows its own session name). */
  bySession: Record<string, CtxReading>;
  /** context-window fullness per NORMALIZED cwd (see normCwd) — the FALLBACK key,
   *  used only for snapshots that carry no tmux session (older agent / no tmux). */
  byCwd: Record<string, CtxReading>;
  /** Fold a status snapshot in. Indexes by tmux session when present (robust) and
   *  ALWAYS also by cwd when present (fallback for tiles that can't match the
   *  session). No-op unless it has a context % and at least one usable key. */
  ingest: (snap: StatusSnapshotWire) => void;
}

export const useSessionContext = create<SessionContextState>((set) => ({
  bySession: {},
  byCwd: {},
  ingest: (snap) =>
    set((s) => {
      // Need an actual context % to show; nothing to record without it.
      if (snap.contextUsedPct == null) return s;
      const reading: CtxReading = {
        usedPct: snap.contextUsedPct,
        ts: snap.ingestedAtMs,
      };

      const session = (snap.tmuxSession ?? "").trim();
      const cwdKey = normCwd(snap.cwd);
      if (!session && !cwdKey) return s; // no key to file it under

      let next = s;
      // Robust index: by tmux session name (`th_<id>`). Freshest wins.
      if (session) {
        const prev = s.bySession[session];
        if (!prev || prev.ts < snap.ingestedAtMs) {
          next = { ...next, bySession: { ...next.bySession, [session]: reading } };
        }
      }
      // Fallback index: by cwd, kept current too so a tile that can't match the
      // session (e.g. before the agent is rebuilt) still reads a value.
      if (cwdKey) {
        const prev = s.byCwd[cwdKey];
        if (!prev || prev.ts < snap.ingestedAtMs) {
          next = { ...next, byCwd: { ...next.byCwd, [cwdKey]: reading } };
        }
      }
      return next;
    }),
}));

/**
 * Look up a tile's context-window fullness (0..=100), preferring the ROBUST tmux
 * binding and falling back to cwd. Pass the tile's terminal id (for its
 * `th_<id>` session name) and its cwd; returns null when neither matches a known
 * session (then the <ContextMeter> renders nothing). Starts the singleton
 * snapshot feed on first use (idempotent), so the meter works without a separate
 * always-mounted subscriber component.
 */
export function useContextPctForTile(
  terminalId: string | undefined,
  cwd: string | undefined,
): number | null {
  ensureFeed();
  const session = terminalId ? sessionNameForTerminal(terminalId) : "";
  const cwdKey = normCwd(cwd);
  return useSessionContext((s) => {
    // Robust first: the reading the agent reported for THIS tile's tmux session.
    const bySession = session ? s.bySession[session] : undefined;
    if (bySession) return bySession.usedPct;
    // Fallback: cwd match (older agent that didn't stamp the session, or no tmux).
    const byCwd = s.byCwd[cwdKey];
    return byCwd ? byCwd.usedPct : null;
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
