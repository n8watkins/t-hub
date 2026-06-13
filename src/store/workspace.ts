// The workspace store holds the live terminal set, focus, and grid order, and
// persists/rehydrates layout so the canvas can reattach after a UI reopen
// (PRD §5.3, §6.5, FR-010). For 0.1, persistence is localStorage; SQLite lands
// later. Only `order` + `focusedId` are persisted — the live `terminals` map is
// re-fetched from the backend on mount via listTerminals().
import { create } from "zustand";
import type { TerminalInfo, TerminalId, TerminalState } from "../ipc/types";

/** localStorage key for the 0.1 layout snapshot. */
const PERSIST_KEY = "termhub.workspace.v1";

/** The subset of state we persist across UI reopens. */
interface PersistedLayout {
  order: TerminalId[];
  focusedId: TerminalId | null;
}

interface WorkspaceState {
  /** Live terminal set, keyed by id (re-fetched from the backend, not persisted). */
  terminals: Record<TerminalId, TerminalInfo>;
  /** Grid order, by terminal id (persisted). */
  order: TerminalId[];
  /** Currently focused tile, or null (persisted). */
  focusedId: TerminalId | null;

  /** Replace the live set from a listTerminals() result, reconciling order/focus. */
  setTerminals: (list: TerminalInfo[]) => void;
  /**
   * Insert a freshly-spawned terminal immediately after the focused tile in
   * `order` (else append), and focus it — PRD §5.3 deterministic insertion.
   */
  addAfterFocused: (info: TerminalInfo) => void;
  /** Drop a terminal from the map + order, moving focus to a neighbor. */
  remove: (id: TerminalId) => void;
  /** Set the focused tile. */
  setFocus: (id: TerminalId) => void;
  /** Update a terminal's lifecycle state from a terminal://state event. */
  updateState: (id: TerminalId, state: TerminalState) => void;
}

/** Read the persisted layout snapshot from localStorage (best-effort). */
function loadPersisted(): PersistedLayout {
  if (typeof localStorage === "undefined") return { order: [], focusedId: null };
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (!raw) return { order: [], focusedId: null };
    const parsed = JSON.parse(raw) as Partial<PersistedLayout>;
    const order = Array.isArray(parsed.order)
      ? parsed.order.filter((id): id is TerminalId => typeof id === "string")
      : [];
    const focusedId =
      typeof parsed.focusedId === "string" && order.includes(parsed.focusedId)
        ? parsed.focusedId
        : null;
    return { order, focusedId };
  } catch {
    return { order: [], focusedId: null };
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
    // Focus wasn't on the removed tile; keep it if still present.
    return currentFocus && nextOrder.includes(currentFocus) ? currentFocus : nextOrder[0] ?? null;
  }
  if (nextOrder.length === 0) return null;
  const idx = prevOrder.indexOf(removedId);
  // Next tile slides into `idx`; if we removed the last one, fall back to previous.
  return nextOrder[idx] ?? nextOrder[idx - 1] ?? nextOrder[0] ?? null;
}

const initial = loadPersisted();

export const useWorkspace = create<WorkspaceState>((set, get) => ({
  terminals: {},
  order: initial.order,
  focusedId: initial.focusedId,

  setTerminals: (list) => {
    const terminals: Record<TerminalId, TerminalInfo> = {};
    for (const t of list) terminals[t.id] = t;

    const prev = get().order;
    const liveIds = new Set(list.map((t) => t.id));
    // Keep the persisted/known order for ids that still exist, in their old
    // positions, then append any newly-seen ids the snapshot didn't know about.
    const kept = prev.filter((id) => liveIds.has(id));
    const known = new Set(kept);
    const appended = list.filter((t) => !known.has(t.id)).map((t) => t.id);
    const order = [...kept, ...appended];

    const focusedId =
      get().focusedId && order.includes(get().focusedId as TerminalId)
        ? get().focusedId
        : order[0] ?? null;

    savePersisted({ order, focusedId });
    set({ terminals, order, focusedId });
  },

  addAfterFocused: (info) => {
    const { order, focusedId, terminals } = get();
    const nextOrder = order.slice();
    const focusIdx = focusedId ? nextOrder.indexOf(focusedId) : -1;
    if (focusIdx >= 0) nextOrder.splice(focusIdx + 1, 0, info.id);
    else nextOrder.push(info.id);

    savePersisted({ order: nextOrder, focusedId: info.id });
    set({
      terminals: { ...terminals, [info.id]: info },
      order: nextOrder,
      focusedId: info.id,
    });
  },

  remove: (id) => {
    const { order, focusedId, terminals } = get();
    const nextOrder = order.filter((x) => x !== id);
    const nextFocus = neighborFocus(order, nextOrder, id, focusedId);

    const nextTerminals = { ...terminals };
    delete nextTerminals[id];

    savePersisted({ order: nextOrder, focusedId: nextFocus });
    set({ terminals: nextTerminals, order: nextOrder, focusedId: nextFocus });
  },

  setFocus: (id) => {
    if (get().focusedId === id) return;
    savePersisted({ order: get().order, focusedId: id });
    set({ focusedId: id });
  },

  updateState: (id, state) => {
    const existing = get().terminals[id];
    if (!existing || existing.state === state) return;
    set({
      terminals: { ...get().terminals, [id]: { ...existing, state } },
    });
  },
}));
