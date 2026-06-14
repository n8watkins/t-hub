// The workspace store holds the live terminal set and a list of user-named
// workspace *tabs* (PRD §5.2). Each tab is its own canvas: an ordered set of
// terminal tiles plus optional manual-mode size ratios for the grid's rows and
// columns (PRD §5.3). Exactly one tab is active; the active tab's canvas is the
// one the user interacts with. Every tab stays MOUNTED at all times (shell v2):
// the canvas toggles inactive tabs with CSS `display:none` and keeps passing
// `visible={true}` to their tiles, so xterm/PTY clients stay attached in the
// background and switching tabs never reloads a terminal. The global terminal
// font size (zoom) is shared by every tile.
//
// Persistence (PRD §6.5, FR-010) is localStorage for now (SQLite lands later):
// we persist the tab list, the active tab, and the font size. The live
// `terminals` map is NOT persisted -- it is re-fetched from the backend on
// mount via listTerminals() and reconciled back onto the persisted tabs.
import { create } from "zustand";
import type { TerminalInfo, TerminalId, TerminalState } from "../ipc/types";

/**
 * localStorage key for the workspace snapshot. v2 introduced workspace tabs;
 * a v1 snapshot (flat order/focus) is migrated into a single tab on load.
 */
const PERSIST_KEY = "termhub.workspace.v2";
const LEGACY_KEY = "termhub.workspace.v1";

/** Global terminal font size (px) bounds + default, shared by every tile. */
const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 28;

/** Default name for the first/auto-created tab. */
const DEFAULT_TAB_NAME = "Workspace 1";

/**
 * Manual-mode size ratios for one tab's grid. `rows` holds a flex-grow weight
 * per grid row; `cols[r]` holds a weight per tile within row `r`. Weights are
 * relative (the grid normalizes them), so any positive numbers work. Empty /
 * missing arrays mean "even split" (default auto-grid behavior).
 */
export interface TabSizes {
  rows: number[];
  cols: number[][];
}

/** A user-named canvas: an ordered tile set plus optional manual size ratios. */
export interface WorkspaceTab {
  id: string;
  name: string;
  /** Tile order within this tab, by terminal id. */
  order: TerminalId[];
  /** Optional manual-mode grid ratios; absent => even auto-grid. */
  sizes?: TabSizes;
}

/** The subset of state we persist across UI reopens. */
interface PersistedLayout {
  tabs: WorkspaceTab[];
  activeTabId: string;
  focusedId: TerminalId | null;
  fontSize: number;
}

interface WorkspaceState {
  /** Live terminal set, keyed by id (re-fetched from the backend, not persisted). */
  terminals: Record<TerminalId, TerminalInfo>;
  /** All workspace tabs, in strip order (persisted). */
  tabs: WorkspaceTab[];
  /** The active tab's id; only its tiles render (persisted). */
  activeTabId: string;
  /** Currently focused tile across the active tab, or null (persisted). */
  focusedId: TerminalId | null;
  /** Global terminal font size in px, applied to every tile equally (persisted). */
  fontSize: number;

  /** Replace the live set from a listTerminals() result, reconciling tabs/order/focus. */
  setTerminals: (list: TerminalInfo[]) => void;
  /** Insert a freshly-spawned terminal after the focused tile in the active tab (else append) and focus it. */
  addAfterFocused: (info: TerminalInfo) => void;
  /** Drop a terminal from every tab + the map, moving focus to a neighbor. */
  remove: (id: TerminalId) => void;
  /** Set the focused tile. */
  setFocus: (id: TerminalId) => void;
  /** Update a terminal's lifecycle state from a terminal://state event. */
  updateState: (id: TerminalId, state: TerminalState) => void;

  // --- Tabs (PRD §5.2) ---
  /** Create a new empty tab (auto-named) and activate it; returns its id. */
  addTab: () => string;
  /** Rename a tab (no-op on blank/unknown id). */
  renameTab: (id: string, name: string) => void;
  /** Close an *empty* tab; refuses if it has tiles or is the last tab. */
  closeTab: (id: string) => void;
  /** Activate a tab (moves focus onto one of its tiles). */
  setActiveTab: (id: string) => void;
  /** Activate the tab at strip index `i` (0-based); no-op if out of range. */
  setActiveTabByIndex: (i: number) => void;
  /** Cycle to the next (+1) / previous (-1) tab, wrapping. */
  cycleTab: (dir: 1 | -1) => void;
  /** Reorder the tab strip: move tab `id` to occupy `targetId`'s slot. */
  moveTab: (id: string, targetId: string) => void;

  // --- Manual layout (PRD §5.3) ---
  /** Reorder tiles within the active tab: pull `id` out and re-insert it at
   *  `targetId`'s position, so a tile can be dropped onto ANY other tile
   *  (including a diagonal grid neighbor), not just an adjacent one. */
  moveTile: (id: TerminalId, targetId: TerminalId) => void;
  /** Persist manual size ratios for a tab. */
  setTabSizes: (id: string, sizes: TabSizes) => void;

  // --- Global zoom ---
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
}

function clampFont(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_FONT_SIZE;
  return Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, Math.round(n)));
}

let tabSeq = 0;
/** Monotonic-ish tab id (timestamp + counter so rapid creates stay unique). */
function newTabId(): string {
  tabSeq += 1;
  return `tab-${Date.now().toString(36)}-${tabSeq.toString(36)}`;
}

/** Sanitize an arbitrary parsed value into a clean order array of string ids. */
function cleanOrder(value: unknown): TerminalId[] {
  return Array.isArray(value)
    ? value.filter((id): id is TerminalId => typeof id === "string")
    : [];
}

/** Sanitize parsed TabSizes; drops anything malformed (=> even split). */
function cleanSizes(value: unknown): TabSizes | undefined {
  if (!value || typeof value !== "object") return undefined;
  const v = value as { rows?: unknown; cols?: unknown };
  const rows = Array.isArray(v.rows)
    ? v.rows.filter((n): n is number => typeof n === "number" && n > 0)
    : [];
  const cols = Array.isArray(v.cols)
    ? v.cols.map((row) =>
        Array.isArray(row)
          ? row.filter((n): n is number => typeof n === "number" && n > 0)
          : [],
      )
    : [];
  if (rows.length === 0 && cols.length === 0) return undefined;
  return { rows, cols };
}

/** Build the default single-tab layout (empty canvas). */
function defaultLayout(): PersistedLayout {
  return {
    tabs: [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }],
    activeTabId: "",
    focusedId: null,
    fontSize: DEFAULT_FONT_SIZE,
  };
}

/**
 * Read the persisted layout. Prefers the v2 (tabbed) snapshot; if absent, a
 * legacy v1 (flat order/focus) snapshot is migrated into a single tab so an
 * upgrading user keeps their terminals. Always returns >=1 tab and a valid
 * activeTabId.
 */
function loadPersisted(): PersistedLayout {
  if (typeof localStorage === "undefined") {
    const d = defaultLayout();
    d.activeTabId = d.tabs[0].id;
    return d;
  }

  const finalize = (layout: PersistedLayout): PersistedLayout => {
    if (layout.tabs.length === 0) {
      layout.tabs = [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }];
    }
    if (!layout.tabs.some((t) => t.id === layout.activeTabId)) {
      layout.activeTabId = layout.tabs[0].id;
    }
    const active = layout.tabs.find((t) => t.id === layout.activeTabId)!;
    if (!layout.focusedId || !active.order.includes(layout.focusedId)) {
      layout.focusedId = active.order[0] ?? null;
    }
    layout.fontSize = clampFont(layout.fontSize);
    return layout;
  };

  // Preferred: v2 tabbed snapshot.
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<PersistedLayout>;
      const tabs: WorkspaceTab[] = Array.isArray(parsed.tabs)
        ? parsed.tabs
            .filter((t): t is WorkspaceTab => !!t && typeof t === "object")
            .map((t) => ({
              id: typeof t.id === "string" && t.id ? t.id : newTabId(),
              name: typeof t.name === "string" && t.name ? t.name : "Workspace",
              order: cleanOrder(t.order),
              sizes: cleanSizes(t.sizes),
            }))
        : [];
      return finalize({
        tabs,
        activeTabId:
          typeof parsed.activeTabId === "string" ? parsed.activeTabId : "",
        focusedId:
          typeof parsed.focusedId === "string" ? parsed.focusedId : null,
        fontSize:
          typeof parsed.fontSize === "number"
            ? parsed.fontSize
            : DEFAULT_FONT_SIZE,
      });
    }
  } catch {
    /* fall through to legacy / default */
  }

  // Migration: legacy v1 flat snapshot -> a single tab.
  try {
    const raw = localStorage.getItem(LEGACY_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as {
        order?: unknown;
        focusedId?: unknown;
        fontSize?: unknown;
      };
      const order = cleanOrder(parsed.order);
      const tab: WorkspaceTab = {
        id: newTabId(),
        name: DEFAULT_TAB_NAME,
        order,
      };
      return finalize({
        tabs: [tab],
        activeTabId: tab.id,
        focusedId:
          typeof parsed.focusedId === "string" ? parsed.focusedId : null,
        fontSize:
          typeof parsed.fontSize === "number"
            ? parsed.fontSize
            : DEFAULT_FONT_SIZE,
      });
    }
  } catch {
    /* fall through to default */
  }

  return finalize(defaultLayout());
}

/** Persist the layout subset (best-effort; ignore quota/serialization errors). */
function savePersisted(layout: PersistedLayout): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(PERSIST_KEY, JSON.stringify(layout));
  } catch {
    /* ignore */
  }
}

/**
 * Pick a sensible focus target after `removedId` leaves `prevOrder`.
 * Prefers the next tile, then the previous, then null. `nextOrder` is the
 * order with `removedId` already removed.
 */
function neighborFocus(
  prevOrder: TerminalId[],
  nextOrder: TerminalId[],
  removedId: TerminalId,
  currentFocus: TerminalId | null,
): TerminalId | null {
  if (currentFocus !== removedId) {
    return currentFocus && nextOrder.includes(currentFocus)
      ? currentFocus
      : nextOrder[0] ?? null;
  }
  if (nextOrder.length === 0) return null;
  const idx = prevOrder.indexOf(removedId);
  return nextOrder[idx] ?? nextOrder[idx - 1] ?? nextOrder[0] ?? null;
}

/** The tab whose `order` contains `id`, or undefined. */
function tabOf(tabs: WorkspaceTab[], id: TerminalId): WorkspaceTab | undefined {
  return tabs.find((t) => t.order.includes(id));
}

const initial = loadPersisted();

export const useWorkspace = create<WorkspaceState>((set, get) => {
  // Persist the current (tabs, activeTabId, focusedId, fontSize).
  const persist = () => {
    const { tabs, activeTabId, focusedId, fontSize } = get();
    savePersisted({ tabs, activeTabId, focusedId, fontSize });
  };

  /** The active tab (always present: the store guarantees >=1 tab). */
  const activeTab = (): WorkspaceTab => {
    const { tabs, activeTabId } = get();
    return tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  };

  return {
    terminals: {},
    tabs: initial.tabs,
    activeTabId: initial.activeTabId,
    focusedId: initial.focusedId,
    fontSize: initial.fontSize,

    setTerminals: (list) => {
      const terminals: Record<TerminalId, TerminalInfo> = {};
      for (const t of list) terminals[t.id] = t;
      const liveIds = new Set(list.map((t) => t.id));

      const { tabs, activeTabId } = get();
      // Keep each tab's ordering for ids that still exist; prune dead ids.
      const placed = new Set<TerminalId>();
      const nextTabs = tabs.map((t) => {
        const order = t.order.filter((id) => liveIds.has(id));
        for (const id of order) placed.add(id);
        return { ...t, order };
      });

      // Any live terminal not already placed in some tab is appended to the
      // active tab (covers first load with pre-existing sessions, or sessions
      // spawned out-of-band by another surface).
      const appended = list.map((t) => t.id).filter((id) => !placed.has(id));
      if (appended.length > 0) {
        const activeIdx = nextTabs.findIndex((t) => t.id === activeTabId);
        const idx = activeIdx >= 0 ? activeIdx : 0;
        nextTabs[idx] = {
          ...nextTabs[idx],
          order: [...nextTabs[idx].order, ...appended],
        };
      }

      const active = nextTabs.find((t) => t.id === activeTabId) ?? nextTabs[0];
      const focusedId =
        get().focusedId && active.order.includes(get().focusedId as TerminalId)
          ? get().focusedId
          : active.order[0] ?? null;

      set({ terminals, tabs: nextTabs, focusedId });
      persist();
    },

    addAfterFocused: (info) => {
      const { tabs, focusedId, terminals } = get();
      const active = activeTab();
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

    remove: (id) => {
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

    setFocus: (id) => {
      if (get().focusedId === id) return;
      set({ focusedId: id });
      persist();
    },

    updateState: (id, state) => {
      const existing = get().terminals[id];
      if (!existing || existing.state === state) return;
      set({
        terminals: { ...get().terminals, [id]: { ...existing, state } },
      });
    },

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

    renameTab: (id, name) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, name: trimmed } : t)) });
      persist();
    },

    closeTab: (id) => {
      const { tabs, activeTabId, focusedId } = get();
      if (tabs.length <= 1) return; // keep at least one tab
      const target = tabs.find((t) => t.id === id);
      if (!target || target.order.length > 0) return; // only close-empty

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
      set({ tabs: nextTabs, activeTabId: nextActive, focusedId: nextFocus });
      persist();
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

    moveTab: (id, targetId) => {
      if (id === targetId) return;
      const { tabs } = get();
      const from = tabs.findIndex((t) => t.id === id);
      const to = tabs.findIndex((t) => t.id === targetId);
      if (from < 0 || to < 0) return;
      const next = tabs.slice();
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      // Reordering doesn't change which tab is active; activeTabId is untouched.
      set({ tabs: next });
      persist();
    },

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

    setTabSizes: (id, sizes) => {
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, sizes } : t)) });
      persist();
    },

    zoomIn: () => {
      set({ fontSize: clampFont(get().fontSize + 1) });
      persist();
    },
    zoomOut: () => {
      set({ fontSize: clampFont(get().fontSize - 1) });
      persist();
    },
    zoomReset: () => {
      set({ fontSize: DEFAULT_FONT_SIZE });
      persist();
    },
  };
});
