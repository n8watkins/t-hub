// Tiles slice: within-tab tile reordering (moveTile), the tile drag-source flag
// (setDraggingTile), cross-tab tile moves (moveTileToTab), and the reserved
// Captains-tab placement primitives (ensureCaptainsTab / moveTileToCaptainsTab /
// moveTileToWorkTab) that designate or un-designate an agent. Action bodies are
// verbatim; cross-slice calls use `get()` and the store closure helpers come via
// `deps`.
import {
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  DEFAULT_TAB_NAME,
  neighborFocus,
  newTabId,
  tabOf,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
  type WorkspaceTab,
} from "./internal";

export const createTilesSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<
  WorkspaceState,
  | "moveTile"
  | "setDraggingTile"
  | "moveTileToTab"
  | "ensureCaptainsTab"
  | "moveTileToCaptainsTab"
  | "moveTileToWorkTab"
> => {
  const { persist, activeTab } = deps;

  return {
    moveTile: (id, targetId) => {
      if (id === targetId) return;
      const { tabs } = get();
      const active = activeTab();
      const from = active.order.indexOf(id);
      const to = active.order.indexOf(targetId);
      if (from < 0 || to < 0) return; // both must be in the active tab

      const order = active.order.slice();
      const [moved] = order.splice(from, 1);
      order.splice(to, 0, moved);

      // Tile count/shape may change rows -> drop stale manual sizes for safety.
      const nextTabs = tabs.map((t) =>
        t.id === active.id ? { ...t, order, sizes: undefined } : t,
      );
      set({ tabs: nextTabs, focusedId: id });
      persist();
    },

    setDraggingTile: (id) => {
      // Transient drag UI only — never persisted. No-op if unchanged so a
      // pointermove-driven re-set doesn't thrash subscribers.
      if (get().draggingTileId === id) return;
      set({ draggingTileId: id });
    },

    moveTileToTab: (id, tabId) => {
      const { tabs, activeTabId, focusedId } = get();
      const source = tabOf(tabs, id);
      if (!source || source.id === tabId) return; // unknown, or already there
      if (!tabs.some((t) => t.id === tabId)) return; // unknown target tab

      // Pull the tile from its source tab and append it to the target tab. Both
      // tabs' manual size ratios are dropped (their grid shapes changed).
      const nextTabs = tabs.map((t) => {
        if (t.id === source.id) {
          return {
            ...t,
            order: t.order.filter((x) => x !== id),
            sizes: undefined,
          };
        }
        if (t.id === tabId) {
          return { ...t, order: [...t.order, id], sizes: undefined };
        }
        return t;
      });

      // If the moved tile was the focused tile of the (still-active) source tab,
      // hand focus to a neighbor; otherwise leave focus + active tab untouched.
      let nextFocus = focusedId;
      if (source.id === activeTabId && focusedId === id) {
        const newOrder = source.order.filter((x) => x !== id);
        nextFocus = neighborFocus(source.order, newOrder, id, focusedId);
      }

      set({ tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    ensureCaptainsTab: () => {
      const { tabs } = get();
      if (tabs.some((t) => t.id === CAPTAINS_TAB_ID)) return CAPTAINS_TAB_ID;
      set({
        tabs: [
          ...tabs,
          {
            schemaVersion: 1,
            id: CAPTAINS_TAB_ID,
            name: CAPTAINS_TAB_NAME,
            kind: "captain",
            order: [],
          },
        ],
      });
      persist();
      return CAPTAINS_TAB_ID;
    },

    moveTileToCaptainsTab: (id) => {
      get().ensureCaptainsTab();
      const { tabs, activeTabId, focusedId } = get();
      const source = tabOf(tabs, id);
      if (source && source.id === CAPTAINS_TAB_ID) return; // already placed

      const nextTabs = tabs.map((t) => {
        if (source && t.id === source.id) {
          return { ...t, order: t.order.filter((x) => x !== id), sizes: undefined };
        }
        if (t.id === CAPTAINS_TAB_ID) {
          const order = t.order.includes(id) ? t.order : [...t.order, id];
          return { ...t, order, sizes: undefined };
        }
        return t;
      });

      // Only touch focus if the moved tile was the active work tab's focused
      // tile - never steal the active tab or the user's view.
      let nextFocus = focusedId;
      if (source && source.id === activeTabId && focusedId === id) {
        const newOrder = source.order.filter((x) => x !== id);
        nextFocus = neighborFocus(source.order, newOrder, id, focusedId);
      }

      set({ tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    moveTileToWorkTab: (id) => {
      const { tabs } = get();
      const captains = tabs.find((t) => t.id === CAPTAINS_TAB_ID);
      if (!captains || !captains.order.includes(id)) return; // not an agent tile

      const target = tabs.find((t) => t.id !== CAPTAINS_TAB_ID);
      if (target) {
        const nextTabs = tabs.map((t) => {
          if (t.id === CAPTAINS_TAB_ID) {
            return {
              ...t,
              order: t.order.filter((x) => x !== id),
              sizes: undefined,
            };
          }
          if (t.id === target.id) {
            return { ...t, order: [...t.order, id], sizes: undefined };
          }
          return t;
        });
        set({ tabs: nextTabs });
      } else {
        // No work tab exists (all-reserved edge): mint a fresh one, work tabs
        // first so the reserved tab stays at the end.
        const fresh: WorkspaceTab = {
          id: newTabId(),
          name: DEFAULT_TAB_NAME,
          order: [id],
        };
        const pruned = tabs.map((t) =>
          t.id === CAPTAINS_TAB_ID
            ? { ...t, order: t.order.filter((x) => x !== id), sizes: undefined }
            : t,
        );
        set({ tabs: [fresh, ...pruned] });
      }
      persist();
    },
  };
};
