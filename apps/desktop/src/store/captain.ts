// The captain store - the "summon the orchestrator" overlay (captain-overlay,
// captain-list phase 1).
//
// Terminals can be PINNED AS CAPTAINS (the orchestrator sessions the general
// talks to - fleet doctrine: one captain per ship, several ships at once). The
// captain overlay stays ONE floating, draggable, resizable panel that renders
// the ACTIVE captain ABOVE whatever workspace tab is active; multi-captain
// means fast switching inside that single panel, not simultaneous panels.
//
// Design notes:
//   - The overlay does NOT create a second attach. The pooled <TerminalView>
//     (TerminalPool #20) stays the single xterm/attach for the session; while
//     the overlay is open it simply OWNS the pool placeholder (the tile copy
//     yields via slotActive, exactly like the fullscreen double-render), so the
//     pooled terminal is repositioned into the overlay and released back to the
//     tile on close. One viewer at a time = no tmux geometry corruption.
//   - `captainIds` is kept in MRU order (index 0 = most recently summoned) and
//     the ACTIVE captain is always the front: explicit summons move-to-front,
//     cycling ROTATES the list (so repeated Ctrl+B C round-robins through every
//     pinned captain instead of ping-ponging between the two most recent).
//   - Persistence follows the workspace-store pattern (lib/persist codec on a
//     versioned localStorage key): the DESIGNATIONS and the overlay GEOMETRY
//     survive an app restart; the open/closed state deliberately does not (the
//     app always starts with the overlay closed). The v1 single-captain blob
//     migrates to a one-entry v2 list - an existing pin is never lost.
//   - Focus contract: opening moves keyboard focus to the captain terminal
//     (via the workspace store's setFocus, which the pooled TerminalView
//     follows); closing restores focus to the tile that had it before. Cycling
//     while summoned re-targets focus but does NOT touch the saved pre-summon
//     tile, so Esc always returns to where the user was before the summon.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";
import { loadPersisted, savePersisted } from "../lib/persist";
import { useWorkspace } from "./workspace";

const PERSIST_KEY = "t-hub.captain.v2";
/** The pre-list single-captain key (PR #9). Read-only now: migrated into the
 *  v2 list on first load, never written again, left in place so a rollback to
 *  an older build still finds its pin. */
const LEGACY_PERSIST_KEY = "t-hub.captain.v1";

/** Overlay size bounds (CSS px). Modest floor so xterm never refits absurdly
 *  small; no ceiling - the container clamp caps it to the canvas. */
export const CAPTAIN_MIN_WIDTH = 360;
export const CAPTAIN_MIN_HEIGHT = 220;
export const CAPTAIN_DEFAULT_WIDTH = 640;
export const CAPTAIN_DEFAULT_HEIGHT = 400;

interface PersistedCaptain {
  /** Pinned captains, MRU order (front = most recently summoned = active). */
  captainIds: TerminalId[];
  /** Overlay top-left, relative to the canvas/pool container. null until the
   *  first open computes a default placement (bottom-right-ish). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;
}

const num = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) ? v : null;

function coerceGeometry(p: {
  x?: unknown;
  y?: unknown;
  width?: unknown;
  height?: unknown;
}): Pick<PersistedCaptain, "x" | "y" | "width" | "height"> {
  return {
    x: num(p.x),
    y: num(p.y),
    width: Math.max(CAPTAIN_MIN_WIDTH, num(p.width) ?? CAPTAIN_DEFAULT_WIDTH),
    height: Math.max(CAPTAIN_MIN_HEIGHT, num(p.height) ?? CAPTAIN_DEFAULT_HEIGHT),
  };
}

/** Sanitize a v2 blob: `captainIds` must be a deduped list of non-empty ids. */
export function coercePersisted(raw: unknown): PersistedCaptain {
  const p = (raw ?? {}) as Partial<PersistedCaptain>;
  const ids = Array.isArray(p.captainIds)
    ? [...new Set(p.captainIds.filter((v): v is string => typeof v === "string" && v !== ""))]
    : [];
  return { captainIds: ids, ...coerceGeometry(p) };
}

/** Convert a v1 single-captain blob (`{ captainId, x, y, width, height }`) into
 *  the v2 list shape: the one pin becomes a one-entry list, geometry carries
 *  over. Exported for the migration tests. */
export function migrateLegacyCaptain(raw: unknown): PersistedCaptain {
  const p = (raw ?? {}) as { captainId?: unknown } & Partial<PersistedCaptain>;
  const captainIds =
    typeof p.captainId === "string" && p.captainId ? [p.captainId] : [];
  return { captainIds, ...coerceGeometry(p) };
}

function defaults(): PersistedCaptain {
  return {
    captainIds: [],
    x: null,
    y: null,
    width: CAPTAIN_DEFAULT_WIDTH,
    height: CAPTAIN_DEFAULT_HEIGHT,
  };
}

/**
 * Load the persisted captain state: the v2 list when present, else the v1
 * single pin migrated into a one-entry list (never losing an existing pin),
 * else empty defaults. A PRESENT v2 blob always wins - an empty v2 list means
 * the user unpinned after migrating, so the stale v1 pin must NOT resurrect.
 * Exported for the migration tests (the store computes this once at load).
 */
export function loadCaptainPersisted(): PersistedCaptain {
  return loadPersisted(
    PERSIST_KEY,
    loadPersisted(LEGACY_PERSIST_KEY, defaults(), migrateLegacyCaptain),
    coercePersisted,
  );
}

const initial = loadCaptainPersisted();

/** The tile focused before the overlay opened, restored on close. Module-level
 *  (not store state): it's transient plumbing, never rendered or persisted. */
let prevFocusedId: TerminalId | null = null;

/** True when `id` currently has a tile in some (non-popped-out) workspace tab -
 *  the pool only renders those, so the overlay can only show those. */
function terminalHasTile(id: TerminalId): boolean {
  return useWorkspace.getState().tabs.some((t) => t.order.includes(id));
}

export interface CaptainState {
  /** Pinned captains in MRU order (front = most recently summoned). Persisted. */
  captainIds: TerminalId[];
  /** The captain the overlay shows / the next summon target. Invariant:
   *  `captainIds[0] ?? null` - kept explicit so every consumer reads one field. */
  activeCaptainId: TerminalId | null;
  /** Whether the overlay is up. Always starts false (not persisted). */
  open: boolean;
  /** Whether the titlebar anchor's captain dropdown is up (not persisted).
   *  Lives here (not component state) so lib/escOverlays can dismiss it from
   *  the single Esc dispatch point without a second listener. */
  anchorMenuOpen: boolean;
  /** Overlay geometry, relative to the canvas container. x/y null = not yet
   *  placed (the overlay computes + commits a default on first open). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;

  /** Pin a captain (ADDITIVE - other pins stay). No-op when already pinned. */
  pinCaptain: (id: TerminalId) => void;
  /** Unpin one captain. Unpinning the ACTIVE captain while summoned closes the
   *  overlay (never show an unpinned/killed session); the next MRU pin becomes
   *  active. Other pins are untouched. */
  unpinCaptain: (id: TerminalId) => void;
  /** Tile-menu / palette toggle: pin this terminal, or unpin it if pinned. */
  toggleCaptain: (id: TerminalId) => void;
  /** Summon a SPECIFIC pinned captain (switcher chip, titlebar dropdown,
   *  palette entry): it becomes active (MRU front), the overlay opens if
   *  closed, and keyboard focus moves to it. No-op if unpinned or tile-less. */
  summonCaptain: (id: TerminalId) => void;
  /** Open the overlay on the first pinned captain (MRU order) that still has a
   *  live tile; no-op when none qualifies. */
  openOverlay: () => void;
  /** Close the overlay and restore focus to the previously focused tile. */
  closeOverlay: () => void;
  /** While summoned: switch to the next pinned captain with a live tile, in
   *  MRU order, by ROTATING the list (round-robin, no ping-pong). No-op when
   *  closed or no other captain qualifies. */
  cycleCaptain: () => void;
  /** The registered Ctrl+B C command: closed -> summon the active captain;
   *  already summoned -> CYCLE to the next captain (Esc dismisses). */
  toggleOverlay: () => void;
  /** Show/hide the titlebar anchor dropdown. */
  setAnchorMenu: (open: boolean) => void;
  /** Commit dragged/resized geometry (persisted). */
  setGeometry: (g: { x: number; y: number; width: number; height: number }) => void;
}

export const useCaptain = create<CaptainState>((set, get) => {
  const persist = () => {
    const s = get();
    savePersisted(PERSIST_KEY, {
      captainIds: s.captainIds,
      x: s.x,
      y: s.y,
      width: s.width,
      height: s.height,
    } satisfies PersistedCaptain);
  };

  /** Set the list + derived active in one shot, keeping the invariant. */
  const commitIds = (captainIds: TerminalId[]) => {
    set({ captainIds, activeCaptainId: captainIds[0] ?? null });
    persist();
  };

  return {
    captainIds: initial.captainIds,
    activeCaptainId: initial.captainIds[0] ?? null,
    open: false,
    anchorMenuOpen: false,
    x: initial.x,
    y: initial.y,
    width: initial.width,
    height: initial.height,

    pinCaptain: (id) => {
      const ids = get().captainIds;
      if (ids.includes(id)) return;
      // New pins land at the END (least recently summoned): pinning is a
      // designation, not a summon, so it never steals the active slot.
      commitIds([...ids, id]);
    },

    unpinCaptain: (id) => {
      const s = get();
      if (!s.captainIds.includes(id)) return;
      // Never leave the overlay showing a session that just lost its pin
      // (kill paths land here via forgetCaptain) - close FIRST so the focus
      // restore runs while the pre-summon tile is still resolvable.
      if (id === s.activeCaptainId && s.open) s.closeOverlay();
      commitIds(s.captainIds.filter((c) => c !== id));
    },

    toggleCaptain: (id) => {
      if (get().captainIds.includes(id)) get().unpinCaptain(id);
      else get().pinCaptain(id);
    },

    summonCaptain: (id) => {
      const s = get();
      if (!s.captainIds.includes(id) || !terminalHasTile(id)) return;
      const ws = useWorkspace.getState();
      // Most recently summoned wins: move to the MRU front.
      commitIds([id, ...s.captainIds.filter((c) => c !== id)]);
      if (!get().open) {
        prevFocusedId = ws.focusedId;
        set({ open: true });
      }
      // Keyboard goes to the captain: the pooled TerminalView focuses its xterm
      // when it becomes the focused tile (Terminal.tsx focus effect).
      ws.setFocus(id);
    },

    openOverlay: () => {
      const { captainIds, open, summonCaptain } = get();
      if (open) return;
      // Summon the first captain (MRU order) whose tile is still live. A pin
      // whose tab popped out to a satellite is skipped, not dropped - it can be
      // summoned again when the tab returns. No live pin: nothing to summon
      // (the titlebar anchor tooltip explains how to pin one).
      const target = captainIds.find((id) => terminalHasTile(id));
      if (target) summonCaptain(target);
    },

    closeOverlay: () => {
      if (!get().open) return;
      set({ open: false });
      // Return focus to the tile that had it before the summon. The saved id
      // can be STALE - that tile may have been closed while the overlay was
      // open - so validate it against the live workspace first and fall back
      // to the active tab's first tile, so focus never stays parked on the
      // (now hidden) captain or on a dead id.
      const ws = useWorkspace.getState();
      const prev = prevFocusedId;
      prevFocusedId = null;
      if (prev && terminalHasTile(prev)) {
        ws.setFocus(prev);
        return;
      }
      const active = ws.tabs.find((t) => t.id === ws.activeTabId);
      const first = active?.order[0];
      if (first) ws.setFocus(first);
    },

    cycleCaptain: () => {
      const s = get();
      if (!s.open) return;
      const ids = s.captainIds;
      const i = Math.max(0, ids.indexOf(s.activeCaptainId ?? ""));
      // Next pinned captain AFTER the active one (wrapping) with a live tile.
      for (let k = 1; k < ids.length; k++) {
        const j = (i + k) % ids.length;
        if (!terminalHasTile(ids[j])) continue;
        // ROTATE so the target becomes the front while the cyclic order is
        // preserved - repeated cycles visit every captain (round-robin)
        // instead of ping-ponging between the two most recent.
        commitIds([...ids.slice(j), ...ids.slice(0, j)]);
        useWorkspace.getState().setFocus(ids[j]);
        return;
      }
      // Solo captain (or no other live one): stay summoned - Esc dismisses.
    },

    toggleOverlay: () => {
      if (get().open) get().cycleCaptain();
      else get().openOverlay();
    },

    setAnchorMenu: (open) => {
      if (get().anchorMenuOpen !== open) set({ anchorMenuOpen: open });
    },

    setGeometry: (g) => {
      set({
        x: g.x,
        y: g.y,
        width: Math.max(CAPTAIN_MIN_WIDTH, g.width),
        height: Math.max(CAPTAIN_MIN_HEIGHT, g.height),
      });
      persist();
    },
  };
});

/**
 * Lifecycle cleanup: when a terminal is killed/removed, unpin it if it was a
 * captain (and drop the overlay if it was the SUMMONED one) so no designation
 * ever points at a dead id. Called from workspace.ts's cleanupTileSideState via
 * dynamic import (matching the DevTab/devserver pattern there - no static cycle
 * with the workspace store).
 */
export function forgetCaptain(id: TerminalId): void {
  useCaptain.getState().unpinCaptain(id);
}
