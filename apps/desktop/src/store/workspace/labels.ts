// Terminal-label slice: the two writers of the effective `labels` map. An
// explicit user rename (`setTerminalLabel`, persisted) always wins over a live
// Claude-suggested title (`setClaudeTitle`, not persisted); both recompute the
// effective map via `mergeLabels`. The `deriveLabel` / `mergeLabels` / label
// helpers themselves are pure and live in `./internal` (re-exported from
// workspace.ts for external consumers). Action bodies are verbatim; only the
// bare `persist()` closure helper is now reached via `deps`.
import type { TerminalId } from "../../ipc/types";
import {
  mergeLabels,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
} from "./internal";

export const createLabelsSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<WorkspaceState, "setTerminalLabel" | "setClaudeTitle"> => {
  const { persist } = deps;

  return {
    setTerminalLabel: (id, label) => {
      const trimmed = label.trim();
      const { userLabels, claudeTitles } = get();
      // Blank clears the override (the Claude title / derived label takes back
      // over); no-op if unchanged so a redundant set doesn't thrash persist.
      let nextUser: Record<TerminalId, string>;
      if (!trimmed) {
        if (!(id in userLabels)) return;
        nextUser = { ...userLabels };
        delete nextUser[id];
      } else {
        if (userLabels[id] === trimmed) return;
        nextUser = { ...userLabels, [id]: trimmed };
      }
      // Recompute the effective map the display reads (rename overlays Claude).
      set({ userLabels: nextUser, labels: mergeLabels(nextUser, claudeTitles) });
      persist();
    },

    setClaudeTitle: (id, title) => {
      const trimmed = title.trim();
      const { userLabels, claudeTitles } = get();
      let nextClaude: Record<TerminalId, string>;
      if (!trimmed) {
        if (!(id in claudeTitles)) return;
        nextClaude = { ...claudeTitles };
        delete nextClaude[id];
      } else {
        if (claudeTitles[id] === trimmed) return;
        nextClaude = { ...claudeTitles, [id]: trimmed };
      }
      // Update the live Claude signal and the effective map. A user rename (in
      // `userLabels`) still wins via mergeLabels, so we never clobber a rename.
      // Not persisted: Claude titles are re-derived from hooks each session.
      set({
        claudeTitles: nextClaude,
        labels: mergeLabels(userLabels, nextClaude),
      });
    },
  };
};
