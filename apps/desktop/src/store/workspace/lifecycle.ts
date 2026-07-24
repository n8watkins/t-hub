// Lifecycle slice: how a tile enters or leaves the layout. Placement of a
// freshly-spawned tile (addAfterFocused / addToTab), the control/MCP spawn path
// (spawnWorkspaceTerminal), adopting the server's authoritative registry snapshot
// (adoptRegistry), removal + its detach/delete/restart variants (remove /
// detachTile / deleteTerminal / restartTerminal), and focusing a tile (setFocus).
// Action bodies are verbatim; cross-slice calls go through `get()` and the store
// closure helpers (`persist`, `activeTab`, `placeWorkTile`, `cleanupTileSideState`,
// `killOldSessionWithRetry`, `captainRegistryIds`) arrive via `deps`.
import {
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  neighborFocus,
  tabOf,
  workspaceKind,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
  type WorkspaceTab,
} from "./internal";

export const createLifecycleSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<
  WorkspaceState,
  | "addAfterFocused"
  | "addToTab"
  | "adoptRegistry"
  | "spawnWorkspaceTerminal"
  | "remove"
  | "detachTile"
  | "deleteTerminal"
  | "restartTerminal"
  | "setFocus"
> => {
  const {
    persist,
    activeTab,
    placeWorkTile,
    cleanupTileSideState,
    killOldSessionWithRetry,
    captainRegistryIds,
  } = deps;

  return {
    addAfterFocused: (info) => {
      const active = activeTab();
      // A plain work spawn must never land in the reserved Captains tab (only
      // agent tiles belong there, via moveTileToCaptainsTab): if the active tab
      // is Captains, redirect the tile into a work tab instead.
      if (workspaceKind(active) === "captain") {
        placeWorkTile(info);
        return;
      }
      const { tabs, focusedId, terminals } = get();
      const nextOrder = active.order.slice();
      const focusIdx = focusedId ? nextOrder.indexOf(focusedId) : -1;
      if (focusIdx >= 0) nextOrder.splice(focusIdx + 1, 0, info.id);
      else nextOrder.push(info.id);

      const nextTabs = tabs.map((t) =>
        t.id === active.id ? { ...t, order: nextOrder } : t,
      );

      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: nextTabs,
        focusedId: info.id,
      });
      persist();
    },

    addToTab: (tabId, info) => {
      // A work tile targeting the reserved Captains tab (e.g. a "+" spawn while
      // Captains is the active tab, which spawnWorkspaceTerminal forwards as the
      // target) is redirected into a work tab - only agent tiles belong in
      // Captains (via moveTileToCaptainsTab).
      if (tabId === CAPTAINS_TAB_ID) {
        placeWorkTile(info);
        return;
      }
      const { tabs, terminals } = get();
      if (!tabs.some((t) => t.id === tabId)) return; // unknown tab: no-op
      const nextTabs = tabs.map((t) =>
        t.id === tabId ? { ...t, order: [...t.order, info.id] } : t,
      );
      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: nextTabs,
        activeTabId: tabId,
        focusedId: info.id,
      });
      persist();
    },

    adoptRegistry: (regTabs) => {
      // Defensive: the server never sends an empty snapshot (close_tab refuses
      // the last tab); an empty one would zero the canvas, so ignore it.
      if (regTabs.length === 0) return;
      // The server has now spoken authoritatively: from here on placement is
      // server-owned, so setTerminals must stop blind-appending unplaced sessions
      // (debris/ghosts) onto the active tab. Set BEFORE the deep-equal early-return
      // below so an echo snapshot that no-ops the tabs still latches the flag.
      if (!get().registryAdopted) set({ registryAdopted: true });
      const { tabs, activeTabId, focusedId, terminals, poppedOutTabs } = get();

      const byId = new Map(tabs.map((t) => [t.id, t]));
      // The reserved Captains tab is CLIENT-ONLY (the backend registry doesn't
      // track it), and its `order` is the authoritative list of tiles placed as
      // agents. Keep an agent tile that is EITHER still reported live by the
      // server (serverTileIds - which includes the reserved tab the reporter
      // up-syncs and the server echoes back) OR still present in the authoritative
      // CAPTAINS REGISTRY (an externally claimed captain - e.g. one the
      // orchestrator claimed over the control socket - whose tile the server does
      // not yet echo as a live work-tab tile). A tile in NEITHER was genuinely
      // closed (server-closed AND released from the registry, which sync_captains
      // keeps in step) and drops out of Captains, cleaned up below like any gone
      // tile. The surviving ids are then held out of the server-derived work tabs
      // so an agent tile never reappears in a work tab after a sync.
      const validRegistryTabs = regTabs.filter(
        (tab) =>
          tab.kind === undefined ||
          tab.kind === (tab.id === CAPTAINS_TAB_ID ? "captain" : "work"),
      );
      const serverTileIds = new Set(validRegistryTabs.flatMap((r) => r.tileIds));
      const registeredCaptains = new Set(captainRegistryIds());
      const localCaptains = tabs.find((t) => t.id === CAPTAINS_TAB_ID);
      const captainsOrder = (localCaptains?.order ?? []).filter(
        (id) => serverTileIds.has(id) || registeredCaptains.has(id),
      );
      // ADOPT an agent the SERVER placed DIRECTLY into the reserved Captains tab
      // that the client never tracked locally - a captain commissioned over the
      // control socket (spawn_terminal with tabId=captains-reserved), whose tile
      // lands in the server's reserved-tab snapshot but is in NEITHER the local
      // captains order (the client never pinned it) NOR any work tab. Without this
      // it is filtered out of every rebuilt tab, so the agents plane renders no
      // tile (the pool renders the union of tab orders) and never attaches a PTY
      // client to it; its live entry (registered by the spawn_terminal apply's
      // adoptTerminal) just lingers unplaced. The KEEP filter above only prunes
      // the existing local order - it can never ADD such a tile - so append it
      // here at the tail (least-recently-summoned, like a fresh local pin),
      // preserving the established order. Idempotent across the reporter round-
      // trip: once adopted it is already in the local order on the next sync.
      //
      // Gate on NOT-already-placed-LOCALLY (any tab), not merely not-in-
      // captainsOrder: when the user unpins a captain, moveTileToWorkTab pulls its
      // tile into a work tab locally BEFORE that layout up-syncs. A server snapshot
      // from the pre-unpin window still lists the tile in captains-reserved; keying
      // on captainsOrder alone would re-adopt it and yank it back into the plane,
      // fighting the user's move. A tile the client already has anywhere is not a
      // new socket commission - skip it and let the normal work-tab/registry paths
      // reconcile.
      // (captainsOrder is a subset of the local captains tab's order, itself a
      // subset of locallyPlaced, so this gate also subsumes not-in-captainsOrder.)
      const locallyPlaced = new Set(tabs.flatMap((t) => t.order));
      const serverCaptainsTiles =
        validRegistryTabs.find((r) => r.id === CAPTAINS_TAB_ID)?.tileIds ?? [];
      for (const id of serverCaptainsTiles) {
        if (!locallyPlaced.has(id)) captainsOrder.push(id);
      }
      const agentSet = new Set(captainsOrder);

      // The reserved Captains tab is CLIENT-ONLY, but the tab reporter up-syncs it
      // to the server like any other tab, so the server echoes it back in this
      // snapshot. Drop every incoming copy of it here (its agent tiles are held in
      // `captainsOrder` and re-appended authoritatively below) - otherwise the
      // echoed copy would render ALONGSIDE the re-appended one as a duplicate tab,
      // and since the echoed copy's tiles are all agent tiles filtered out by
      // `agentSet`, that duplicate has an empty `order` and shows the stray "new
      // terminal" placeholder even though the real Captains tab has terminals.
      const serverTabs: WorkspaceTab[] = validRegistryTabs
        .filter(
          (r) =>
            (r.kind ?? (r.id === CAPTAINS_TAB_ID ? "captain" : "work")) !==
            "captain",
        )
        .map((r) => {
          const existing = byId.get(r.id);
          const order = r.tileIds.filter((id) => !agentSet.has(id));
          const sameOrder =
            existing !== undefined &&
            existing.order.length === order.length &&
            existing.order.every((x, i) => x === order[i]);
          return {
            schemaVersion: 1,
            id: r.id,
            name: r.name.trim() || existing?.name || "Workspace",
            kind: "work",
            order,
            // Manual grid ratios survive only if the tile set didn't change.
            sizes: sameOrder ? existing.sizes : undefined,
          };
        });
      // Re-append the reserved Captains tab (never dropped by a server sync).
      const nextTabs: WorkspaceTab[] = [
        ...serverTabs,
        {
          id: CAPTAINS_TAB_ID,
          name: CAPTAINS_TAB_NAME,
          schemaVersion: 1,
          kind: "captain",
          order: captainsOrder,
          sizes:
            localCaptains &&
            localCaptains.order.length === captainsOrder.length &&
            localCaptains.order.every((x, i) => x === captainsOrder[i])
              ? localCaptains.sizes
              : undefined,
        },
      ];

      // Deep-equal snapshots are a no-op (apply echoes must not churn persist /
      // the tab reporter).
      const unchanged =
        nextTabs.length === tabs.length &&
        nextTabs.every((t, i) => {
          const o = tabs[i];
          return (
            t.id === o.id &&
            t.name === o.name &&
            t.schemaVersion === o.schemaVersion &&
            workspaceKind(t) === workspaceKind(o) &&
            t.order.length === o.order.length &&
            t.order.every((x, j) => x === o.order[j])
          );
        });
      if (unchanged) return;

      // Keep the user's view valid but NEVER steal it: activeTabId moves only if
      // its tab was closed; focus moves only if the focused tile left the active
      // tab.
      let nextActive = activeTabId;
      if (!nextTabs.some((t) => t.id === nextActive)) nextActive = nextTabs[0].id;
      const active = nextTabs.find((t) => t.id === nextActive)!;
      const nextFocus =
        focusedId && active.order.includes(focusedId)
          ? focusedId
          : active.order[0] ?? null;

      // Tiles gone from every rendered tab (and not popped out) were closed
      // headlessly - drop their live entries + side state like closeTab does.
      const after = new Set(nextTabs.flatMap((t) => t.order));
      const popped = new Set(poppedOutTabs.flatMap((t) => t.order));
      const nextTerminals = { ...terminals };
      for (const t of tabs) {
        for (const id of t.order) {
          if (!after.has(id) && !popped.has(id)) {
            delete nextTerminals[id];
            cleanupTileSideState(id);
          }
        }
      }

      set({
        tabs: nextTabs,
        activeTabId: nextActive,
        focusedId: nextFocus,
        terminals: nextTerminals,
      });
      persist();
    },

    spawnWorkspaceTerminal: async (opts) => {
      // Capture the target tab id SYNCHRONOUSLY (before the async spawn) so a focus
      // change mid-spawn can't misplace the tile.
      const tabId = opts?.tabId ?? get().activeTabId;
      try {
        const { spawnTerminal } = await import("../../ipc/client");
        const info = await spawnTerminal({
          cwd: opts?.cwd?.trim() || undefined,
          name: opts?.name?.trim() || undefined,
          shell: opts?.shell?.trim() || undefined,
          startupCommand: opts?.startupCommand?.trim() || undefined,
        });
        if (get().tabs.some((t) => t.id === tabId)) get().addToTab(tabId, info);
        else get().addAfterFocused(info);
        return info.id;
      } catch (err) {
        console.error("spawnWorkspaceTerminal failed", err);
        return null;
      }
    },

    remove: (id) => {
      // The tile is going away (detach + delete both funnel here) -> drop its
      // external per-tile state (panel view + any managed dev server). Not
      // reached by a tab MOVE, so a moved tile keeps its panel state.
      cleanupTileSideState(id);
      const { tabs, focusedId, terminals, activeTabId } = get();
      const owner = tabOf(tabs, id);
      const nextTabs = tabs.map((t) =>
        t.order.includes(id)
          ? { ...t, order: t.order.filter((x) => x !== id) }
          : t,
      );

      // Only recompute focus if the removed tile lived in the active tab.
      let nextFocus = focusedId;
      if (owner && owner.id === activeTabId) {
        const prevOrder = owner.order;
        const newOrder = prevOrder.filter((x) => x !== id);
        nextFocus = neighborFocus(prevOrder, newOrder, id, focusedId);
      }

      const nextTerminals = { ...terminals };
      delete nextTerminals[id];

      set({ terminals: nextTerminals, tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    detachTile: (id) => {
      // Detach the PTY client but DO NOT kill tmux: the backing session keeps
      // running so the terminal can be re-adopted later. Drop the tile from the
      // layout immediately; the backend call is fire-and-forget. The dynamic
      // import keeps the store free of a hard Tauri dependency (web/test safe),
      // matching saveToBackend's pattern.
      void import("../../ipc/client")
        .then((m) => m.closeTerminal(id))
        .catch((err) => console.error("closeTerminal failed", err));
      get().remove(id);
    },

    deleteTerminal: (id) => {
      // Destructive: kill the tmux session for good (terminates its process
      // tree) via the backend, then refresh History from the surviving provider
      // transcript. Dynamic imports keep the store web/test safe.
      void import("../../ipc/client")
        .then((m) => m.killTerminal(id))
        .then(() => import("../../ipc/history"))
        .then((m) => m.invalidateHistoryCache())
        .catch((err) => console.error("killTerminal or History refresh failed", err))
        .finally(() => {
          if (typeof window !== "undefined") {
            window.dispatchEvent(new Event("t-hub:history-changed"));
          }
        });
      get().remove(id);
    },

    restartTerminal: async (id) => {
      // Recover a frozen session: spawn a FRESH tmux session in the same cwd,
      // swap it into the OLD tile's exact tab + slot, then kill the old session.
      // Reuses the same spawnTerminal / killTerminal IPCs the "+" and "×" use, so
      // there is no new tmux logic and the tile lands exactly where it was.
      const info = get().terminals[id];
      if (!info) return null;
      const cwd = (info.cwd ?? "").trim();
      try {
        const { spawnTerminal, killTerminal } = await import("../../ipc/client");
        const fresh = await spawnTerminal({ cwd: cwd || undefined });
        // Drop the old tile's per-tile side state (panel view, dev server,
        // captain pin, context reading, …) BEFORE the swap — same cleanup
        // `remove` runs, but here we place the fresh tile in the vacated slot
        // rather than closing the cell.
        cleanupTileSideState(id);
        const s = get();
        let placed = false;
        const nextTabs = s.tabs.map((t) => {
          const at = t.order.indexOf(id);
          if (at === -1) return t;
          const order = [...t.order];
          order[at] = fresh.id; // in-place: SAME slot the old tile held
          placed = true;
          return { ...t, order };
        });
        // Fallback: if the old tile had already left every tab mid-restart, drop
        // the fresh tile into the active tab so it is never orphaned.
        if (!placed) {
          const active = s.activeTabId;
          for (let i = 0; i < nextTabs.length; i++) {
            if (nextTabs[i].id === active) {
              nextTabs[i] = {
                ...nextTabs[i],
                order: [...nextTabs[i].order, fresh.id],
              };
              break;
            }
          }
        }
        const nextTerminals = { ...s.terminals, [fresh.id]: fresh };
        delete nextTerminals[id];
        set({
          tabs: nextTabs,
          terminals: nextTerminals,
          focusedId: s.focusedId === id ? fresh.id : s.focusedId,
        });
        persist();
        // Kill the OLD tmux session (process tree) now that the tile is replaced.
        // Fire-and-forget relative to the swap (which already stands), but retry
        // once and surface a visible notice on a second failure — otherwise a
        // failed kill leaks a frozen tmux session with no tile bound to it.
        void killOldSessionWithRetry(id, killTerminal);
        return fresh.id;
      } catch (err) {
        // The only await before the synchronous swap is the spawn, so a throw
        // here means the fresh session never came up: the old tile is untouched.
        console.error("restartTerminal failed", err);
        return null;
      }
    },

    setFocus: (id) => {
      // Focusing a tile implies the user is working in the canvas, so navigation
      // focus returns to the terminal region (so a subsequent Ctrl+Tab cycles
      // terminals, and Ctrl+B toggles back to the sidebar). Only `focusedId` is
      // persisted; the region is transient.
      const cur = get();
      if (cur.focusedId === id) {
        if (cur.focusedRegion !== "terminal") set({ focusedRegion: "terminal" });
        return;
      }
      set({ focusedId: id, focusedRegion: "terminal" });
      persist();
    },
  };
};
