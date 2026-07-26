// Tabs slice: everything that creates, names, activates, cycles, reorders, closes,
// or tears off a workspace tab, plus the drag-target highlight setters and the
// per-tab size ratios. This is the largest slice. Action bodies are moved verbatim
// from the former god-store; cross-slice calls go through `get()` (e.g.
// `get().moveTileToCaptainsTab`, `get().setFocus`), and the store closure's
// `persist()` / `activeTab()` helpers are reached via `deps`.
import { useTheme } from "../theme";
import {
  CAPTAINS_TAB_ID,
  DEFAULT_TAB_NAME,
  newTabId,
  workspaceKind,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
  type WorkspaceTab,
} from "./internal";

export const createTabsSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<
  WorkspaceState,
  | "addTab"
  | "adoptTab"
  | "ensureTab"
  | "renameTab"
  | "closeTab"
  | "closeWorkspace"
  | "closeEmptyWorkspaces"
  | "setActiveTab"
  | "setActiveTabByIndex"
  | "cycleTab"
  | "cycleTile"
  | "moveTab"
  | "popOutTab"
  | "popInTab"
  | "setTabSizes"
  | "setDraggingTab"
  | "setDropTab"
  | "setDropTile"
> => {
  const { persist, activeTab, cleanupTileSideState, agentPresentationIds } = deps;

  return {
    addTab: () => {
      const { tabs } = get();
      // Auto-name "Workspace N" using the lowest free index.
      const used = new Set(
        tabs
          .map((t) => /^Workspace (\d+)$/.exec(t.name)?.[1])
          .filter((n): n is string => !!n)
          .map((n) => Number(n)),
      );
      let n = 1;
      while (used.has(n)) n += 1;
      const tab: WorkspaceTab = {
        id: newTabId(),
        name: `Workspace ${n}`,
        order: [],
      };
      set({ tabs: [...tabs, tab], activeTabId: tab.id, focusedId: null });
      persist();
      return tab.id;
    },

    adoptTab: (id, name) => {
      const { tabs } = get();
      if (tabs.some((t) => t.id === id)) {
        set({ activeTabId: id });
        return;
      }
      const tab: WorkspaceTab = { id, name: name.trim() || "Workspace", order: [] };
      set({ tabs: [...tabs, tab], activeTabId: id, focusedId: null });
      persist();
    },

    ensureTab: (id, name) => {
      const { tabs } = get();
      const byId = tabs.find((t) => t.id === id);
      if (byId) return byId.id;
      const byName = tabs.find((t) => t.name === name);
      if (byName) return byName.id;
      const tab: WorkspaceTab = { id, name: name.trim() || "Workspace", order: [] };
      set({ tabs: [...tabs, tab], activeTabId: id, focusedId: null });
      persist();
      return id;
    },

    renameTab: (id, name) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, name: trimmed } : t)) });
      persist();
    },

    closeWorkspace: (id) => {
      // The reserved Captains tab is never closeable.
      if (id === CAPTAINS_TAB_ID) return;
      // Tier 3 reap. ONLY the workspace × calls this; switch/pop-out never do, so
      // they can't kill. Mirror closeTab's last-tab guard BEFORE killing so a kill
      // never fires when closeTab would refuse to remove the tab.
      const { tabs } = get();
      // Guard on the WORK-tab count: the reserved Captains tab is ALWAYS present,
      // so it must not count toward the last-tab check - else the last work tab
      // could be closed, parking the user on the Captains-only view.
      if (tabs.filter((t) => workspaceKind(t) === "work").length <= 1) return;
      const target = tabs.find((t) => t.id === id);
      if (!target) return;

      // PROTECT CAPTAINS (protective default): a registered-captain tile that
      // happens to sit in this work tab must NEVER be SIGKILLed by a workspace
      // close - a captain is a long-lived orchestrator, and this exact vector
      // (closeWorkspace reaping a captain tile mis-placed in a work tab) killed a
      // live captain during a re-org. Re-place it into the reserved Captains tab
      // instead, and kill only the genuine work sessions. (The precise UX - silent
      // re-place vs. a confirm prompt - is flagged for the general's ratification;
      // the protective default ships now.)
      const registeredCaptains = new Set(agentPresentationIds());
      const captainsHere = target.order.filter((tid) => registeredCaptains.has(tid));
      for (const tid of captainsHere) get().moveTileToCaptainsTab(tid);
      const ids = target.order.filter((tid) => !registeredCaptains.has(tid));

      const refreshRecent = (): void => {
        if (typeof window !== "undefined") {
          window.dispatchEvent(new Event("t-hub:recent-changed"));
        }
      };
      const refreshHistory = (): void => {
        if (typeof window !== "undefined") {
          window.dispatchEvent(new Event("t-hub:history-changed"));
        }
      };
      // RECALL-FIRST: drop the daemon's Recent cache, THEN force the re-fetch — the
      // dispatch is chained AFTER the invalidate resolves so RecentList re-scans a
      // freshly-dropped cache, not the stale 15s-TTL one (a brand-new project closed
      // within 15s of its first scan would otherwise lag). On a failure we still
      // refresh (the on-disk transcript — Recent's source of truth — survives the
      // SIGKILL, so `claude --resume` works regardless, and the open-cwd filter
      // un-hides the closed projects synchronously via closeTab below).
      void import("../../ipc/recent")
        .then((m) => m.invalidateRecentCache())
        .then(refreshRecent)
        .catch((err) => {
          console.error("invalidateRecentCache failed", err);
          refreshRecent();
        });

      // SIGKILL each WORK session's process tree via the SAME backend path the
      // per-tile × uses (killTerminal → kill_terminal → tmux::kill_session_tree).
      // `ids` excludes any registered-captain tile (re-placed above), so a captain
      // is never reaped here. Fire-and-forget (mirrors deleteTerminal); a kill error
      // is logged, not surfaced.
      void import("../../ipc/client").then((m) => {
        void Promise.allSettled(ids.map((tid) => m.killTerminal(tid)))
          .then(() => import("../../ipc/history"))
          .then((history) => history.invalidateHistoryCache())
          .catch((err) => console.error("History refresh failed (closeWorkspace)", err))
          .finally(refreshHistory);
      }).catch((err) => {
        console.error("killTerminal import failed (closeWorkspace)", err);
        refreshHistory();
      });

      // Layout removal/prune/persist (also deletes the tiles from `terminals`, so
      // RecentList's open-cwd filter reactively un-hides the closed projects — the
      // immediate visible recall, independent of the cache re-fetch above).
      get().closeTab(id);
    },

    closeEmptyWorkspaces: () => {
      const { tabs, activeTabId } = get();
      const workTabs = tabs.filter((tab) => workspaceKind(tab) === "work");
      if (workTabs.length <= 1) return [];

      const emptyTabs = workTabs.filter((tab) => tab.order.length === 0);
      if (emptyTabs.length === 0) return [];

      // If every workspace is empty, keep the active work workspace. If the
      // active tab is somehow the reserved Captain Workspace, keep the first
      // work workspace instead. Otherwise every empty workspace is removable
      // because a non-empty work workspace remains.
      const keepId =
        emptyTabs.length === workTabs.length
          ? workTabs.find((tab) => tab.id === activeTabId)?.id ?? workTabs[0].id
          : null;
      const closeIds = emptyTabs
        .map((tab) => tab.id)
        .filter((id) => id !== keepId);

      // Reuse closeTab's active-neighbor selection, color cleanup, persistence,
      // and last-workspace guard. These tabs have no tile IDs, so this path can
      // never detach or kill a process.
      for (const id of closeIds) get().closeTab(id);
      return closeIds;
    },

    closeTab: (id) => {
      const { tabs, activeTabId, focusedId, terminals } = get();
      if (id === CAPTAINS_TAB_ID) return []; // reserved: never closeable
      // Keep at least one WORK tab: the reserved Captains tab is always present
      // and must not count toward the guard, so closing the last work tab is
      // refused (else the user is parked on the Captains-only view).
      if (tabs.filter((t) => workspaceKind(t) === "work").length <= 1) return [];
      const target = tabs.find((t) => t.id === id);
      if (!target) return [];

      // Tiles this tab held; returned so the caller can detach their terminals
      // (closeTerminal — tmux survives). Also dropped from the live map here so
      // the canvas stops rendering them once the tab is gone.
      const removed = target.order.slice();

      const idx = tabs.findIndex((t) => t.id === id);
      const nextTabs = tabs.filter((t) => t.id !== id);

      // If we closed the active tab, activate a neighbor.
      let nextActive = activeTabId;
      let nextFocus = focusedId;
      if (activeTabId === id) {
        const neighbor = nextTabs[idx] ?? nextTabs[idx - 1] ?? nextTabs[0];
        nextActive = neighbor.id;
        nextFocus = neighbor.order[0] ?? null;
      }

      const nextTerminals = { ...terminals };
      for (const tid of removed) {
        delete nextTerminals[tid];
        cleanupTileSideState(tid); // closing the tab takes its tiles with it
      }
      // The tab is gone for good — drop its color identity so a recycled tab id
      // can't inherit it. (A POP-OUT keeps the record, so popOutTab must NOT.)
      useTheme.getState().clearWorkspaceColor(id);

      set({
        terminals: nextTerminals,
        tabs: nextTabs,
        activeTabId: nextActive,
        focusedId: nextFocus,
      });
      persist();
      return removed;
    },

    setActiveTab: (id) => {
      const { tabs, activeTabId } = get();
      if (id === activeTabId) return;
      const tab = tabs.find((t) => t.id === id);
      if (!tab) return;
      set({ activeTabId: id, focusedId: tab.order[0] ?? null });
      persist();
    },

    setActiveTabByIndex: (i) => {
      const { tabs, activeTabId } = get();
      const tab = tabs[i];
      if (!tab || tab.id === activeTabId) return;
      set({ activeTabId: tab.id, focusedId: tab.order[0] ?? null });
      persist();
    },

    cycleTab: (dir) => {
      const { tabs, activeTabId } = get();
      if (tabs.length <= 1) return;
      const idx = tabs.findIndex((t) => t.id === activeTabId);
      const nextIdx = (idx + dir + tabs.length) % tabs.length;
      const next = tabs[nextIdx];
      set({ activeTabId: next.id, focusedId: next.order[0] ?? null });
      persist();
    },

    cycleTile: (dir) => {
      const order = activeTab().order;
      if (order.length <= 1) return;
      const { focusedId } = get();
      const cur = focusedId ? order.indexOf(focusedId) : -1;
      const base = cur >= 0 ? cur : 0;
      const nextIdx = (base + dir + order.length) % order.length;
      const nextId = order[nextIdx];
      if (nextId === focusedId) return;
      // Reuse setFocus so navigation focus snaps back to the terminal region.
      get().setFocus(nextId);
    },

    moveTab: (id, targetId) => {
      if (id === targetId) return;
      const { tabs } = get();
      const from = tabs.findIndex((t) => t.id === id);
      const to = tabs.findIndex((t) => t.id === targetId);
      if (from < 0 || to < 0) return;
      const next = tabs.slice();
      const [moved] = next.splice(from, 1);
      // Insert at the TARGET's slot regardless of drag direction. Removing an
      // earlier source (from < to) shifts the target down one, so without this
      // adjustment a downward move would land one slot PAST the target (the
      // off-by-one the insertion highlight didn't match). Upward moves are
      // unaffected (from > to => adj === to).
      const adj = from < to ? to - 1 : to;
      next.splice(adj, 0, moved);
      // Reordering doesn't change which tab is active; activeTabId is untouched.
      set({ tabs: next });
      persist();
    },

    popOutTab: (id) => {
      const { tabs, poppedOutTabs, activeTabId, focusedId } = get();
      const tab = tabs.find((t) => t.id === id);
      if (!tab) return; // unknown / already popped out
      // Move the record out of the rendered set so the strip + canvas drop it.
      const nextTabs = tabs.filter((t) => t.id !== id);
      const nextPopped = poppedOutTabs.some((t) => t.id === id)
        ? poppedOutTabs
        : [...poppedOutTabs, tab];

      // Keep >=1 rendered tab. If this was the only tab, leave a fresh empty one
      // so the main window still has a canvas to work with.
      const renderedTabs =
        nextTabs.length > 0
          ? nextTabs
          : [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }];

      // If the popped tab was active, hand activeness to a still-rendered tab.
      let nextActive = activeTabId;
      let nextFocus = focusedId;
      if (activeTabId === id) {
        nextActive = renderedTabs[0].id;
        nextFocus = renderedTabs[0].order[0] ?? null;
      }
      set({
        tabs: renderedTabs,
        poppedOutTabs: nextPopped,
        activeTabId: nextActive,
        focusedId: nextFocus,
      });
      persist();
    },

    popInTab: (id, tab) => {
      const { tabs, poppedOutTabs } = get();
      const stashed = poppedOutTabs.find((t) => t.id === id);
      // Nothing to re-adopt, or it's somehow already visible: clear any stash.
      if (!stashed && !tab) return;
      if (tabs.some((t) => t.id === id)) {
        set({ poppedOutTabs: poppedOutTabs.filter((t) => t.id !== id) });
        persist();
        return;
      }
      const record = tab ?? stashed!;
      set({
        tabs: [...tabs, record],
        poppedOutTabs: poppedOutTabs.filter((t) => t.id !== id),
      });
      persist();
    },

    setDraggingTab: (id) => {
      if (get().draggingTabId === id) return;
      set({ draggingTabId: id });
    },
    setDropTile: (id) => {
      if (get().dropTileId === id) return;
      set({ dropTileId: id });
    },
    setDropTab: (id) => {
      if (get().dropTabId === id) return;
      set({ dropTabId: id });
    },

    setTabSizes: (id, sizes) => {
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, sizes } : t)) });
      persist();
    },
  };
};
