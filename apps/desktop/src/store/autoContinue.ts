// Per-terminal "auto-continue on usage limit" coverage — DEFAULT ON, opt-out.
//
// When a terminal's agent session RUNS OUT of usage — either the on-screen Claude
// usage-limit MODAL appears (pane-text trigger) or a rate-limit window hits its
// cap and carries a reset time (statusline/Codex timer trigger) — T-Hub recovers
// it automatically: dismiss the dialog (ESC) and inject the continue command so
// the agent picks the work back up on its own. src/lib/autoContinueMount watches
// for both triggers and does the recovery.
//
// Coverage is DEFAULT ON fleet-wide: every terminal is watched unless the user
// OPTED IT OUT via the tile ⋯ menu. This store records the opt-OUT set (the
// inverse of the old opt-in map); `isWatched` is the single question the mount +
// the tile UI ask. Persisted so the choice survives an app restart while the
// tmux session (and thus the terminal id) lives.
//
// MIGRATION — the shape changed (opt-in map → opt-out set), so this uses a NEW
// versioned key (v2). The old v1 key was an opt-IN map; under default-ON its
// contents are moot (an opted-in tile is watched anyway, and the tiles NOT in it
// are now watched too), so we deliberately do NOT read it as if it were opt-out.
// v2 simply starts empty (everyone watched); the stale v1 key is left in place
// for rollback safety, never read here.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";

const STORAGE_KEY = "t-hub.autoContinue.v2";

function load(): Record<TerminalId, true> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const v = JSON.parse(raw) as unknown;
    // v2 shape: { optedOut: { [id]: true } }. Tolerate a bare id→bool map too.
    const bag =
      v && typeof v === "object" && "optedOut" in (v as object)
        ? (v as { optedOut?: unknown }).optedOut
        : v;
    if (bag && typeof bag === "object") {
      const out: Record<string, true> = {};
      for (const [k, off] of Object.entries(bag as Record<string, unknown>)) {
        if (off === true) out[k] = true; // store only the opted-OUT ids
      }
      return out;
    }
  } catch {
    /* corrupt / unavailable — start empty (everyone watched) */
  }
  return {};
}

function save(optedOut: Record<TerminalId, true>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ optedOut }));
  } catch {
    /* ignore quota / serialization errors */
  }
}

interface AutoContinueState {
  /** terminalId -> true when the user OPTED THIS TILE OUT of auto-continue. A
   *  tile absent from this map is WATCHED (the default). */
  optedOut: Record<TerminalId, true>;
  /** Whether auto-continue is active for a tile: watched unless opted out. */
  isWatched: (id: TerminalId) => boolean;
  /** Flip a tile between watched (default) and opted-out. */
  toggle: (id: TerminalId) => void;
  /** Set a tile's coverage explicitly (watched = true, opted-out = false). */
  setWatched: (id: TerminalId, watched: boolean) => void;
}

export const useAutoContinue = create<AutoContinueState>((set, get) => ({
  optedOut: load(),
  isWatched: (id) => get().optedOut[id] !== true,
  toggle: (id) =>
    set((s) => {
      const next = { ...s.optedOut };
      if (next[id]) delete next[id];
      else next[id] = true;
      save(next);
      return { optedOut: next };
    }),
  setWatched: (id, watched) =>
    set((s) => {
      const next = { ...s.optedOut };
      if (watched) delete next[id];
      else next[id] = true;
      save(next);
      return { optedOut: next };
    }),
}));
