// The captain store - the "summon the orchestrator" overlay (captain-overlay).
//
// One terminal can be PINNED AS CAPTAIN (the orchestrator session the general
// talks to). The captain overlay is a floating, draggable, resizable panel that
// renders that terminal ABOVE whatever workspace tab is active, so the captain
// is reachable from anywhere without switching tabs.
//
// Design notes:
//   - The overlay does NOT create a second attach. The pooled <TerminalView>
//     (TerminalPool #20) stays the single xterm/attach for the session; while
//     the overlay is open it simply OWNS the pool placeholder (the tile copy
//     yields via slotActive, exactly like the fullscreen double-render), so the
//     pooled terminal is repositioned into the overlay and released back to the
//     tile on close. One viewer at a time = no tmux geometry corruption.
//   - Persistence follows the workspace-store pattern (lib/persist codec on a
//     versioned localStorage key): the DESIGNATION and the overlay GEOMETRY
//     survive an app restart; the open/closed state deliberately does not (the
//     app always starts with the overlay closed).
//   - Focus contract: opening moves keyboard focus to the captain terminal
//     (via the workspace store's setFocus, which the pooled TerminalView
//     follows); closing restores focus to the tile that had it before.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";
import { loadPersisted, savePersisted } from "../lib/persist";
import { useWorkspace } from "./workspace";

const PERSIST_KEY = "t-hub.captain.v1";

/** Overlay size bounds (CSS px). Modest floor so xterm never refits absurdly
 *  small; no ceiling - the container clamp caps it to the canvas. */
export const CAPTAIN_MIN_WIDTH = 360;
export const CAPTAIN_MIN_HEIGHT = 220;
export const CAPTAIN_DEFAULT_WIDTH = 640;
export const CAPTAIN_DEFAULT_HEIGHT = 400;

interface PersistedCaptain {
  captainId: TerminalId | null;
  /** Overlay top-left, relative to the canvas/pool container. null until the
   *  first open computes a default placement (bottom-right-ish). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;
}

function coercePersisted(raw: unknown): PersistedCaptain {
  const p = (raw ?? {}) as Partial<PersistedCaptain>;
  const num = (v: unknown): number | null =>
    typeof v === "number" && Number.isFinite(v) ? v : null;
  return {
    captainId: typeof p.captainId === "string" && p.captainId ? p.captainId : null,
    x: num(p.x),
    y: num(p.y),
    width: Math.max(CAPTAIN_MIN_WIDTH, num(p.width) ?? CAPTAIN_DEFAULT_WIDTH),
    height: Math.max(CAPTAIN_MIN_HEIGHT, num(p.height) ?? CAPTAIN_DEFAULT_HEIGHT),
  };
}

function defaults(): PersistedCaptain {
  return {
    captainId: null,
    x: null,
    y: null,
    width: CAPTAIN_DEFAULT_WIDTH,
    height: CAPTAIN_DEFAULT_HEIGHT,
  };
}

const initial = loadPersisted(PERSIST_KEY, defaults(), coercePersisted);

/** The tile focused before the overlay opened, restored on close. Module-level
 *  (not store state): it's transient plumbing, never rendered or persisted. */
let prevFocusedId: TerminalId | null = null;

/** True when `id` currently has a tile in some (non-popped-out) workspace tab -
 *  the pool only renders those, so the overlay can only show those. */
function terminalHasTile(id: TerminalId): boolean {
  return useWorkspace.getState().tabs.some((t) => t.order.includes(id));
}

export interface CaptainState {
  /** The pinned captain terminal, or null when none is designated. Persisted. */
  captainId: TerminalId | null;
  /** Whether the overlay is up. Always starts false (not persisted). */
  open: boolean;
  /** Overlay geometry, relative to the canvas container. x/y null = not yet
   *  placed (the overlay computes + commits a default on first open). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;

  /** Pin (or with null, unpin) the captain. Unpinning closes the overlay. */
  setCaptain: (id: TerminalId | null) => void;
  /** Tile-menu toggle: pin this terminal, or unpin it if already the captain. */
  toggleCaptain: (id: TerminalId) => void;
  /** Open the overlay (no-op without a live captain) and focus the captain. */
  openOverlay: () => void;
  /** Close the overlay and restore focus to the previously focused tile. */
  closeOverlay: () => void;
  /** The registered command / titlebar-anchor action. */
  toggleOverlay: () => void;
  /** Commit dragged/resized geometry (persisted). */
  setGeometry: (g: { x: number; y: number; width: number; height: number }) => void;
}

export const useCaptain = create<CaptainState>((set, get) => {
  const persist = () => {
    const s = get();
    savePersisted(PERSIST_KEY, {
      captainId: s.captainId,
      x: s.x,
      y: s.y,
      width: s.width,
      height: s.height,
    } satisfies PersistedCaptain);
  };

  return {
    captainId: initial.captainId,
    open: false,
    x: initial.x,
    y: initial.y,
    width: initial.width,
    height: initial.height,

    setCaptain: (id) => {
      if (id === null && get().open) get().closeOverlay();
      set({ captainId: id });
      persist();
    },

    toggleCaptain: (id) => {
      get().setCaptain(get().captainId === id ? null : id);
    },

    openOverlay: () => {
      const { captainId, open } = get();
      if (open) return;
      // No captain, or its tile is gone (killed / popped out with its tab):
      // nothing to summon. The titlebar anchor tooltip explains how to pin one.
      if (!captainId || !terminalHasTile(captainId)) return;
      const ws = useWorkspace.getState();
      prevFocusedId = ws.focusedId;
      set({ open: true });
      // Keyboard goes to the captain: the pooled TerminalView focuses its xterm
      // when it becomes the focused tile (Terminal.tsx focus effect).
      ws.setFocus(captainId);
    },

    closeOverlay: () => {
      if (!get().open) return;
      set({ open: false });
      // Return focus to the tile that had it before the summon (if it still
      // exists); otherwise leave focus wherever it is.
      const prev = prevFocusedId;
      prevFocusedId = null;
      if (prev && terminalHasTile(prev)) useWorkspace.getState().setFocus(prev);
    },

    toggleOverlay: () => {
      if (get().open) get().closeOverlay();
      else get().openOverlay();
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
 * Lifecycle cleanup: when a terminal is killed/removed, unpin it if it was the
 * captain (and drop the overlay) so the designation never points at a dead id.
 * Called from workspace.ts's cleanupTileSideState via dynamic import (matching
 * the DevTab/devserver pattern there - no static cycle with the workspace store).
 */
export function forgetCaptain(id: TerminalId): void {
  const s = useCaptain.getState();
  if (s.captainId !== id) return;
  s.setCaptain(null); // also closes the overlay if open
}
