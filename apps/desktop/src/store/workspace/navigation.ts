// Navigation slice: keyboard-nav region focus (setFocusRegion / toggleFocusRegion)
// and the cross-tab global tile ring (cycleTileGlobal). Action bodies are verbatim;
// cross-slice calls (setActiveTab / setFocus) go through `get()`. No store closure
// helpers are needed here, so `deps` is unused.
import type { TerminalId } from "../../ipc/types";
import {
  type FocusRegion,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
} from "./internal";

export const createNavigationSlice = (
  set: StoreSet,
  get: StoreGet,
  _deps: SliceDeps,
): Pick<
  WorkspaceState,
  "setFocusRegion" | "toggleFocusRegion" | "cycleTileGlobal"
> => {
  void _deps;

  return {
    setFocusRegion: (region) => {
      if (get().focusedRegion === region) return;
      set({ focusedRegion: region });
    },

    toggleFocusRegion: () => {
      const next: FocusRegion =
        get().focusedRegion === "sidebar" ? "terminal" : "sidebar";
      set({ focusedRegion: next });
      return next;
    },

    cycleTileGlobal: (dir) => {
      const { tabs, activeTabId, focusedId } = get();
      // Flatten EVERY tab's tile order (strip order) into one global ring, each
      // entry tagged with its owning tab so we can switch tabs when we cross a
      // boundary. Popped-out tabs live in other windows, so they're excluded.
      const flat: { id: TerminalId; tabId: string }[] = [];
      for (const t of tabs) {
        for (const id of t.order) flat.push({ id, tabId: t.id });
      }
      if (flat.length <= 1) return;
      // Locate the current focused tile in the global ring (prefer the entry in
      // the ACTIVE tab when an id somehow appears twice; ids are unique across
      // tabs in practice, but this keeps the step deterministic). Default to the
      // start so an unset focus still cycles.
      let cur = flat.findIndex(
        (e) => e.id === focusedId && e.tabId === activeTabId,
      );
      if (cur < 0) cur = flat.findIndex((e) => e.id === focusedId);
      const base = cur >= 0 ? cur : 0;
      const next = flat[(base + dir + flat.length) % flat.length];
      if (next.id === focusedId && next.tabId === activeTabId) return;
      // Cross a workspace boundary by activating the owning tab first, then focus
      // the tile. setActiveTab re-points focus to that tab's first tile; the
      // following setFocus overrides it with the exact target AND snaps the nav
      // focus back to the terminal region.
      if (next.tabId !== activeTabId) get().setActiveTab(next.tabId);
      get().setFocus(next.id);
    },
  };
};
