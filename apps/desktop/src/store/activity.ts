// Per-terminal output-activity tracker — a lightweight "is this terminal actively
// producing output right now?" signal. It drives the sidebar row's RUNNING
// animation for agents that have NO hook-based mid-turn signal (Codex; also plain
// shells running a command), complementing Claude's supervision-driven pulse
// (store/supervision.ts tmuxSessionMidTurn).
//
// Design: bump(id) is called on every output chunk. It resets a per-terminal idle
// timer (cheap — no React render) and only writes the store on a TRANSITION:
// idle→active on the first chunk, active→idle after IDLE_MS of silence. So an
// actively streaming terminal causes exactly two row re-renders across an entire
// burst, not one per chunk.
import { create } from "zustand";

/** No output for this long ⇒ the terminal is considered idle and the pulse stops.
 *  Long enough to bridge a mid-turn pause (e.g. Codex running a tool/command, which
 *  emits no output for a few seconds) without the animation flickering off and back
 *  on; short enough that the pulse settles soon after a turn really ends. */
const IDLE_MS = 2500;

interface ActivityState {
  /** terminalId → currently producing output. */
  active: Record<string, boolean>;
  /** Record an output chunk for `id`. Safe to call on every chunk (throttled
   *  internally to two state writes per active burst). */
  bump: (id: string) => void;
  /** Drop a terminal's activity entry when its tile goes away for good (close /
   *  detach / close-tab). `active[id]` is set on every output chunk and only ever
   *  flipped to false (never deleted), so without this it grows once per spawned
   *  terminal. Deletes the `active[id]` key and clears any pending idle timer so a
   *  late-firing timeout can't resurrect a stale key after the tile is gone. */
  forget: (id: string) => void;
}

/** Per-terminal idle timers. Module-level (not in the store) so resetting them on
 *  every chunk never triggers a render — only the active↔idle flips below do. */
const idleTimers = new Map<string, ReturnType<typeof setTimeout>>();

export const useActivity = create<ActivityState>((set, get) => ({
  active: {},
  bump: (id) => {
    const existing = idleTimers.get(id);
    if (existing) clearTimeout(existing);
    idleTimers.set(
      id,
      setTimeout(() => {
        idleTimers.delete(id);
        if (get().active[id]) {
          set((s) => ({ active: { ...s.active, [id]: false } }));
        }
      }, IDLE_MS),
    );
    // Only re-render on the idle→active edge; subsequent chunks just reset the
    // timer above.
    if (!get().active[id]) {
      set((s) => ({ active: { ...s.active, [id]: true } }));
    }
  },
  forget: (id) => {
    // Clear any pending idle timer so it can't fire after teardown and re-seed
    // the key we're about to delete.
    const t = idleTimers.get(id);
    if (t) {
      clearTimeout(t);
      idleTimers.delete(id);
    }
    if (!(id in get().active)) return; // nothing to drop (avoid a redundant write)
    set((s) => {
      const active = { ...s.active };
      delete active[id];
      return { active };
    });
  },
}));
