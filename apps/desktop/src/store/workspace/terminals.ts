// Live-terminal-set slice: reconcile the backend's terminal enumeration onto the
// tabs (setTerminals), refresh live metadata (updateTerminalsMeta), register a
// server-spawned terminal (adoptTerminal), and apply a lifecycle-state event
// (updateState). Action bodies are moved verbatim from the former god-store; the
// only mechanical change is that the store closure's `persist()` / `activeTab()`
// helpers are now reached through `deps`.
import type { TerminalInfo, TerminalId } from "../../ipc/types";
import {
  DEFAULT_TAB_NAME,
  newTabId,
  workspaceKind,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
  type WorkspaceTab,
} from "./internal";

export const createTerminalsSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<
  WorkspaceState,
  "setTerminals" | "updateTerminalsMeta" | "adoptTerminal" | "updateState"
> => {
  const { persist, captainRegistryIds, satelliteTab: SATELLITE_TAB } = deps;

  return {
    setTerminals: (list) => {
      const terminals: Record<TerminalId, TerminalInfo> = {};
      for (const t of list) terminals[t.id] = t;
      const liveIds = new Set(list.map((t) => t.id));

      const { tabs, activeTabId, poppedOutTabs, registryAdopted } = get();
      // Once the server registry is authoritative, terminal enumeration updates
      // runtime metadata only. A temporarily empty or partial scan must not turn
      // into a layout mutation that the tab reporter sends back to the server.
      // Before adoption, retain the legacy liveness pruning used to repair a
      // persisted local-only layout.
      const placed = new Set<TerminalId>();
      const registeredCaptains = new Set(captainRegistryIds());
      const recoveredFromCaptain: TerminalId[] = [];
      const nextTabs = tabs.map((t) => {
        let order = registryAdopted
          ? t.order
          : t.order.filter((id) => liveIds.has(id));
        // A registry-less boot can briefly append every live shell to the
        // active Captain Workspace before bootstrap creates the first Work
        // Workspace. Keep durable Captain tiles pinned, but recover ordinary
        // shells into a work tab so the next report is valid.
        if (!registryAdopted && workspaceKind(t) === "captain") {
          const captainOrder: TerminalId[] = [];
          for (const id of order) {
            if (registeredCaptains.has(id)) captainOrder.push(id);
            else recoveredFromCaptain.push(id);
          }
          order = captainOrder;
        }
        for (const id of order) placed.add(id);
        return { ...t, order };
      });

      // Popped-out tabs live in other windows but their terminals are still in
      // the backend's list. Prune their orders to live ids and count them as
      // PLACED so they aren't yanked back onto this window's active tab below.
      const nextPopped = poppedOutTabs.map((t) => {
        const order = t.order.filter((id) => liveIds.has(id));
        for (const id of order) placed.add(id);
        return { ...t, order };
      });
      let nextActiveTabId = activeTabId;

      // Any live terminal not already placed in some tab is appended to the
      // active tab (covers first load with pre-existing sessions, or sessions
      // spawned out-of-band by another surface). NOT in a satellite window: its
      // unplaced terminals belong to the OTHER windows' tabs, so adopting them
      // would drag every session into the satellite. A satellite shows exactly
      // the tiles its own tab record lists.
      //
      // AUTHORITATIVE PLACEMENT (adopt-harden): once the SERVER has delivered its
      // registry (`registryAdopted`), placement is server-owned - `adoptRegistry`
      // rebuilds the tabs from the authoritative snapshot. A live `th_*` session
      // that the server does NOT place is either debris (e.g. a leaked churn-test
      // ghost) or a not-yet-adopted tile the server will place on its own; in
      // NEITHER case may it be blind-dumped onto the active tab. That blind-append
      // is exactly the gate that adopted 13 ghost sessions onto the canvas and
      // blanked the UI. Keep the legacy append ONLY until the first registry
      // arrives (a registry-less boot), so pre-existing sessions still surface.
      const appended =
        SATELLITE_TAB || registryAdopted
          ? []
          : [...recoveredFromCaptain, ...list.map((t) => t.id)].filter(
              (id, index, all) => !placed.has(id) && all.indexOf(id) === index,
            );
      if (appended.length > 0) {
        let idx = nextTabs.findIndex(
          (t) => t.id === activeTabId && workspaceKind(t) === "work",
        );
        if (idx < 0) idx = nextTabs.findIndex((t) => workspaceKind(t) === "work");
        if (idx < 0) {
          const fresh: WorkspaceTab = {
            id: newTabId(),
            name: DEFAULT_TAB_NAME,
            order: [],
          };
          idx = nextTabs.length - 1;
          nextTabs.splice(idx, 0, fresh);
        }
        nextTabs[idx] = {
          ...nextTabs[idx],
          order: [...nextTabs[idx].order, ...appended],
        };
        nextActiveTabId = nextTabs[idx].id;
      }

      // SATELLITE blank-boot (#4): DEFERRED — needs scoped recovery, not the
      // unscoped list. An earlier attempt repopulated an empty satellite tab from
      // `list.map(t => t.id)`, but `list` (listTerminals) is EVERY window's
      // sessions, so the satellite would adopt the MAIN window's terminals and a
      // second tmux client would attach to each → interleaved/garbled output (the
      // exact case the `appended` block above avoids for satellites). We can't
      // scope by id once the tab's own ids pruned away (they're gone), so a correct
      // fix needs per-terminal owning-tab metadata from the backend (or a
      // persist-before-pop-out guarantee). Until then a satellite that pruned to
      // empty stays empty (pre-v0.3.20 behavior) rather than dual-attaching.

      const active = nextTabs.find((t) => t.id === nextActiveTabId) ?? nextTabs[0];
      const focusedId =
        get().focusedId && active.order.includes(get().focusedId as TerminalId)
          ? get().focusedId
          : active.order[0] ?? null;

      set({
        terminals,
        tabs: nextTabs,
        activeTabId: nextActiveTabId,
        poppedOutTabs: nextPopped,
        focusedId,
      });
      persist();
    },

    updateTerminalsMeta: (list) => {
      const { terminals } = get();
      let changed = false;
      const next: Record<TerminalId, TerminalInfo> = { ...terminals };
      for (const t of list) {
        const ex = next[t.id];
        if (!ex) continue; // unknown id: new terminals arrive via setTerminals
        // Overwrite cwd with the backend's value (which `list_terminals` fills
        // from the pane's LIVE `#{pane_current_path}`), so `terminals[id].cwd`
        // tracks the CURRENT pane directory — refreshed on the ~5s poll — not
        // just the spawn dir. We keep the single `cwd` field (no separate
        // spawn/live field): the spawn value seeds it and is then replaced live,
        // so existing `cwd` consumers (Files tree root, worktree anchor) read the
        // live path with no rename. Title/state ride along on the same diff.
        if (ex.cwd !== t.cwd || ex.title !== t.title || ex.state !== t.state) {
          next[t.id] = { ...ex, cwd: t.cwd, title: t.title, state: t.state };
          changed = true;
        }
      }
      // No order/focus change and NOT persisted (live metadata only): avoids
      // thrashing the layout snapshot on every poll.
      if (changed) set({ terminals: next });
    },

    adoptTerminal: (info) => {
      const { terminals } = get();
      if (terminals[info.id]) return;
      // Live map only: placement/persist ride the registry snapshot adopt.
      set({ terminals: { ...terminals, [info.id]: info } });
    },

    updateState: (id, state) => {
      const existing = get().terminals[id];
      if (existing) {
        if (existing.state === state) return;
        set({
          terminals: { ...get().terminals, [id]: { ...existing, state } },
        });
        return;
      }
      // No record yet, but the terminal lives in a tab (it was restored from a
      // persisted layout): a terminal://state event raced ahead of the
      // listTerminals() seed. Upsert a minimal record so the transition isn't
      // dropped -- otherwise an attach's `live` event arriving before setTerminals
      // is lost and the tile stays stuck on the amber "starting" fallback (#16).
      // Ignore states for ids we don't track at all (no tab, no record).
      const { tabs } = get();
      if (!tabs.some((t) => t.order.includes(id))) return;
      set({
        terminals: {
          ...get().terminals,
          [id]: { id, tmuxSession: `th_${id}`, cwd: "", title: id, state },
        },
      });
    },
  };
};
