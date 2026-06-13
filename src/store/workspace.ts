// The workspace store holds the live terminal set, focus, grid order, and the
// global terminal font size (zoom), and persists/rehydrates that layout so the
// canvas can reattach after a UI reopen (PRD §5.3, §6.5, FR-010). For 0.1,
// persistence is localStorage; SQLite lands later. Only `order`, `focusedId`,
// and `fontSize` are persisted -- the live `terminals` map is re-fetched from
// the backend on mount via listTerminals().
import { create } from "zustand";
import type { TerminalInfo, TerminalId, TerminalState } from "../ipc/types";

/** localStorage key for the 0.1 layout snapshot. */
const PERSIST_KEY = "termhub.workspace.v1";

/** Global terminal font size (px) bounds + default, shared by every tile. */
const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 28;

/** The subset of state we persist across UI reopens. */
interface PersistedLayout {
  order: TerminalId[];
  focusedId: TerminalId | null;
  fontSize: number;
}

interface WorkspaceState {
  /** Live terminal set, keyed by id (re-fetched from the backend, not persisted). */
  terminals: Record<TerminalId, TerminalInfo>;
  /** Grid order, by terminal id (persisted). */
  order: TerminalId[];
  /** Currently focused tile, or null (persisted). */
  focusedId: TerminalId | null;
  /** Global terminal font size in px, applied to every tile equally (persisted). */
  fontSize: number;

  /** Replace the live set from a listTerminals() result, reconciling order/focus. */
  setTerminals: (list: TerminalInfo[]) => void;
  /** Insert a freshly-spawned terminal after the focused tile (else append) and focus it. */
  addAfterFocused: (info: TerminalInfo) => void;
  /** Drop a terminal from the map + order, moving focus to a neighbor. */
  remove: (id: TerminalId) => void;
  /** Set the focused tile. */
  setFocus: (id: TerminalId) => void;
  /** Update a terminal's lifecycle state from a terminal://state event. */
  updateState: (id: TerminalId, state: TerminalState) => void;
  /** Global zoom: bump every terminal's font size up/down or reset. */
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
}

function clampFont(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_FONT_SIZE;
  return Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, Math.round(n)));
}

/** Read the persisted layout snapshot from localStorage (best-effort). */
function loadPersisted(): PersistedLayout {
  const fallback: PersistedLayout = {
    order: [],
    focusedId: null,
    fontSize: DEFAULT_FONT_SIZE,
  };
  if (typeof localStorage === "undefined") return fallback;
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<PersistedLayout>;
    const order = Array.isArray(parsed.order)
      ? parsed.order.filter((id): id is TerminalId => typeof id === "string")
      : [];
    const focusedId =
      typeof parsed.focusedId === "string" && order.includes(parsed.focusedId)
        ? parsed.focusedId
        : null;
    const fontSize =
      typeof parsed.fontSize === "number"
        ? clampFont(parsed.fontSize)
        : DEFAULT_FONT_SIZE;
    return { order, focusedId, fontSize };
  } catch {
    return fallback;
  }
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

const initial = loadPersisted();

export const useWorkspace = create<WorkspaceState>((set, get) => {
  // Persist the current (order, focusedId, fontSize) triple.
  const persist = () => {
    const { order, focusedId, fontSize } = get();
    savePersisted({ order, focusedId, fontSize });
  };

  return {
    terminals: {},
    order: initial.order,
    focusedId: initial.focusedId,
    fontSize: initial.fontSize,

    setTerminals: (list) => {
      const terminals: Record<TerminalId, TerminalInfo> = {};
      for (const t of list) terminals[t.id] = t;

      const prev = get().order;
      const liveIds = new Set(list.map((t) => t.id));
      const kept = prev.filter((id) => liveIds.has(id));
      const known = new Set(kept);
      const appended = list.filter((t) => !known.has(t.id)).map((t) => t.id);
      const order = [...kept, ...appended];

      const focusedId =
        get().focusedId && order.includes(get().focusedId as TerminalId)
          ? get().focusedId
          : order[0] ?? null;

      set({ terminals, order, focusedId });
      persist();
    },

    addAfterFocused: (info) => {
      const { order, focusedId, terminals } = get();
      const nextOrder = order.slice();
      const focusIdx = focusedId ? nextOrder.indexOf(focusedId) : -1;
      if (focusIdx >= 0) nextOrder.splice(focusIdx + 1, 0, info.id);
      else nextOrder.push(info.id);

      set({
        terminals: { ...terminals, [info.id]: info },
        order: nextOrder,
        focusedId: info.id,
      });
      persist();
    },

    remove: (id) => {
      const { order, focusedId, terminals } = get();
      const nextOrder = order.filter((x) => x !== id);
      const nextFocus = neighborFocus(order, nextOrder, id, focusedId);

      const nextTerminals = { ...terminals };
      delete nextTerminals[id];

      set({ terminals: nextTerminals, order: nextOrder, focusedId: nextFocus });
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
