// Per-terminal "auto-continue on usage reset" opt-in.
//
// When a terminal's Claude session RUNS OUT of usage (a rate-limit window hits
// its cap), TermHub can wait until that window RESETS and then inject a continue
// command so the agent picks the work back up on its own — for the terminals the
// user has enabled this on. This store records WHICH terminals are opted in;
// src/lib/autoContinueMount watches the statusline snapshots and does the timing
// + injection. Persisted so the choice survives an app restart while the tmux
// session (and thus the terminal id) lives.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";

const STORAGE_KEY = "termhub.autoContinue.v1";

function load(): Record<TerminalId, boolean> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const v = JSON.parse(raw) as unknown;
    if (v && typeof v === "object") {
      const out: Record<string, boolean> = {};
      for (const [k, on] of Object.entries(v as Record<string, unknown>)) {
        if (on === true) out[k] = true; // store only the enabled ids
      }
      return out;
    }
  } catch {
    /* corrupt / unavailable — start empty */
  }
  return {};
}

function save(enabled: Record<TerminalId, boolean>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(enabled));
  } catch {
    /* ignore quota / serialization errors */
  }
}

interface AutoContinueState {
  /** terminalId -> true when auto-continue-on-reset is enabled for that tile. */
  enabled: Record<TerminalId, boolean>;
  toggle: (id: TerminalId) => void;
  setEnabled: (id: TerminalId, on: boolean) => void;
}

export const useAutoContinue = create<AutoContinueState>((set, get) => ({
  enabled: load(),
  toggle: (id) =>
    set((s) => {
      const next = { ...s.enabled };
      if (next[id]) delete next[id];
      else next[id] = true;
      save(next);
      return { enabled: next };
    }),
  setEnabled: (id, on) =>
    set((s) => {
      const next = { ...s.enabled };
      if (on) next[id] = true;
      else delete next[id];
      save(next);
      return { enabled: next };
    }),
}));
