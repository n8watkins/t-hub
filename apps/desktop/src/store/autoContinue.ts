// Per-terminal usage-reset alerts. Retains the legacy preference key and
// opt-outs; terminal recovery never injects automatic keystrokes.
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
