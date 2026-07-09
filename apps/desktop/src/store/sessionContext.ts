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
// STRICTLY PER-SESSION binding: the agent stamps the OWNING TMUX SESSION onto
// each statusline before journaling — it reads `$TMUX_PANE` (the pane the
// statusline runs in) and resolves `#{session_name}` on the `t-hub` socket.
// T-Hub names every session `th_<terminalId>`, so the frontend keys context by
// that session name and a tile looks itself up by its OWN `th_<id>` (which it can
// compute directly). This is the ONLY key — it is precise even when two tiles
// share a directory.
//
// NO cwd fallback (glitch-header): an earlier build ALSO indexed each reading by
// normalized cwd as a fallback for un-upgraded agents. That was a CROSS-SESSION
// LEAK: two sessions in the SAME directory (e.g. a captain and a crew both in the
// main worktree) share one `byCwd` bucket, so one session's reading surfaced on
// the OTHER's tile — most visibly right after an app restart, before a tile's own
// snapshot re-lands. Every T-Hub session runs under tmux and stamps
// `tmux_session`, so the fallback protected a case that cannot occur here while
// silently mis-attributing readings across sessions. Showing the WRONG session's
// number is worse than showing none, so the meter now binds ONLY by the owning
// tmux session; a reading with no `tmux_session` is dropped rather than guessed.
//
// It deliberately keeps its OWN `status://snapshot` subscription (server-split
// M1: the snapshot now arrives over the control socket and the demux hub fans it
// to every `onControlEvent` subscriber) rather than reshaping the supervision
// store, which keys by session id and carries none of this — staying out of that
// store keeps this feature self-contained and cleanly revertible.
import { create } from "zustand";
import { onStatus, type StatusSnapshotWire } from "../ipc/client05";

/** The tmux session name T-Hub gives a terminal: `th_<terminalId>`. This is
 *  the binding key — a tile computes it from its own id and looks up the
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
  /** context-window fullness per TMUX SESSION NAME (`th_<id>`) — the owning
   *  session key the agent reports. This is the ONLY index; a tile reads its own
   *  `th_<id>`, so a reading can never surface on a tile that merely shares the
   *  reporting session's directory. */
  bySession: Record<string, CtxReading>;
  /** Fold a status snapshot in. Files it under the owning tmux session only.
   *  A reading we cannot attribute to a specific session (no `tmux_session`) is
   *  dropped, never guessed by cwd. A FRESHER snapshot for a session we already
   *  track that carries NO context % RESETS that session's reading (the /clear
   *  fix) rather than leaving the old, now-wrong number stale. */
  ingest: (snap: StatusSnapshotWire) => void;
  /** Drop a terminal's context reading when its tile goes away for good (close /
   *  detach / close-tab). Deletes the `bySession` entry for the terminal's
   *  `th_<id>` session name so this map can't grow without bound across spawns. */
  forget: (terminalId: string) => void;
}

export const useSessionContext = create<SessionContextState>((set) => ({
  bySession: {},
  ingest: (snap) =>
    set((s) => {
      const session = (snap.tmuxSession ?? "").trim();
      // No owning session → we cannot bind it to a tile without guessing (which
      // would leak across same-cwd tiles). Drop it.
      if (!session) return s;
      const prev = s.bySession[session];
      if (prev && prev.ts >= snap.ingestedAtMs) return s; // stale/out-of-order
      // No context % on a FRESHER snapshot for a session we track = its context
      // was RESET (e.g. `/clear`, which empties the window so the statusline
      // stops reporting a `context_window`). Drop the stale reading so the meter
      // clears immediately instead of pinning the old, now-wrong number until the
      // next turn repopulates it. Nothing tracked yet → nothing to reset.
      if (snap.contextUsedPct == null) {
        if (!prev) return s;
        const bySession = { ...s.bySession };
        delete bySession[session];
        return { bySession };
      }
      const reading: CtxReading = {
        usedPct: snap.contextUsedPct,
        ts: snap.ingestedAtMs,
      };
      return { bySession: { ...s.bySession, [session]: reading } };
    }),
  forget: (terminalId) =>
    set((s) => {
      const session = sessionNameForTerminal(terminalId);
      if (!(session in s.bySession)) return s; // nothing filed under this session
      const bySession = { ...s.bySession };
      delete bySession[session];
      return { bySession };
    }),
}));

/**
 * Pure lookup of a tile's context-window fullness (0..=100) from a store
 * snapshot, keyed STRICTLY by the tile's owning tmux session (`th_<id>`).
 * Returns null when this session has reported no reading yet. Exported so the
 * hook and its test share one definition.
 */
export function readContextPct(
  state: Pick<SessionContextState, "bySession">,
  terminalId: string | undefined,
): number | null {
  if (!terminalId) return null;
  const reading = state.bySession[sessionNameForTerminal(terminalId)];
  return reading ? reading.usedPct : null;
}

/**
 * Look up a tile's context-window fullness (0..=100), keyed STRICTLY by the
 * tile's own tmux session (`th_<id>`). Returns null when this tile's session has
 * reported no reading (then the <ContextMeter> renders nothing). Starts the
 * singleton snapshot feed on first use (idempotent), so the meter works without a
 * separate always-mounted subscriber component.
 */
export function useContextPctForTile(
  terminalId: string | undefined,
): number | null {
  ensureFeed();
  return useSessionContext((s) => readContextPct(s, terminalId));
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
