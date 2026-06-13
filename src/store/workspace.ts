// STUB — implemented by the Canvas subagent (task #11).
//
// The workspace store holds the live terminal set, focus, and grid order, and
// persists/rehydrates layout so the canvas can reattach after a UI reopen
// (PRD §6.5, FR-010). For 0.1, persistence may be localStorage; SQLite lands later.
import { create } from "zustand";
import type { TerminalInfo, TerminalId } from "../ipc/types";

interface WorkspaceState {
  terminals: TerminalInfo[];
  /** Grid order, by terminal id. */
  order: TerminalId[];
  focusedId: TerminalId | null;
}

export const useWorkspace = create<WorkspaceState>(() => ({
  terminals: [],
  order: [],
  focusedId: null,
}));
