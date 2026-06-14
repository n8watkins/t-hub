// Persistent terminal pool (#20 — seamless moves).
//
// Problem: TermHub keeps every tab mounted, but a terminal's xterm instance is
// created by <TerminalView> *inside* its grid cell. Moving a tile to another tab
// reparents that TerminalView (tab A's grid -> tab B's grid), which remounts and
// reattaches xterm — a visible reload/flash. Moving/resizing into a different
// slot makes xterm refit mid-relayout and the content squishes briefly.
//
// Fix: render each terminal's <TerminalView> EXACTLY ONCE, in a single
// absolutely-positioned pool layer that NEVER changes parent. The grid cells keep
// their header chrome and expose an empty, ref'd *placeholder* box where the
// terminal body should sit. A layout-sync effect positions each pooled terminal
// (position:absolute; left/top/width/height) on top of its current placeholder.
// Because the pooled TerminalView keeps a stable React key + parent across tab
// moves, reorders and resizes, xterm is never remounted/reattached — a move only
// repositions the existing instance. Terminal.tsx's own ResizeObserver refits it
// when its box changes size, so we never touch Terminal.tsx.
//
// Placeholders that belong to an inactive tab (or that don't exist this render,
// e.g. a terminal that has no tile yet) are kept mounted but hidden
// (visibility:hidden) and parked offscreen so they stay attached in the
// background, exactly like before.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useWorkspace } from "../store/workspace";
import { TerminalView } from "./Terminal";
import type { TerminalId } from "../ipc/types";

// ---------------------------------------------------------------------------
// Placeholder registry (context). Each Tile registers the empty body box it
// renders, keyed by terminal id; the pool reads the registry to know where to
// position each pooled terminal. A monotonic `version` bumps whenever the set of
// placeholders changes (mount/unmount) so the pool re-syncs immediately.
// ---------------------------------------------------------------------------

interface PoolRegistry {
  /** Register (or replace) the body placeholder element for a terminal id. */
  register: (id: TerminalId, el: HTMLElement) => void;
  /** Remove a terminal id's placeholder (on tile unmount). */
  unregister: (id: TerminalId, el: HTMLElement) => void;
  /** Ask the pool to re-measure now (e.g. after a layout-affecting change). */
  requestSync: () => void;
}

const PoolContext = createContext<PoolRegistry | null>(null);

/**
 * Register this element as terminal `id`'s body placeholder for the lifetime of
 * the calling component. The pool positions `id`'s pooled terminal over it.
 * No-op outside a <TerminalPoolProvider> (so Tile still works standalone).
 */
export function useTerminalSlot(id: TerminalId) {
  const reg = useContext(PoolContext);
  const ref = useRef<HTMLDivElement | null>(null);
  const setRef = useCallback(
    (el: HTMLDivElement | null) => {
      const prev = ref.current;
      if (prev === el) return;
      if (prev && reg) reg.unregister(id, prev);
      ref.current = el;
      if (el && reg) reg.register(id, el);
    },
    [id, reg],
  );
  return setRef;
}

// ---------------------------------------------------------------------------
// Provider + pool layer.
// ---------------------------------------------------------------------------

export interface TerminalPoolProps {
  children: React.ReactNode;
}

/**
 * Wraps the canvas body. Renders the children (the tab grids, which contain the
 * tile headers + placeholders) and, on top of them, the pooled terminal layer.
 * Both share this single `position:relative` box so a placeholder's rect maps
 * directly onto the absolute coordinates of its pooled terminal.
 */
export function TerminalPoolProvider({ children }: TerminalPoolProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  // id -> placeholder element. A plain ref (not state) so registration never
  // re-renders the provider; the pool re-syncs off the `version` bump instead.
  const slotsRef = useRef<Map<TerminalId, HTMLElement>>(new Map());
  const [version, setVersion] = useState(0);
  const bump = useCallback(() => setVersion((v) => v + 1), []);

  const registry = useMemo<PoolRegistry>(
    () => ({
      register: (id, el) => {
        slotsRef.current.set(id, el);
        bump();
      },
      unregister: (id, el) => {
        // Only delete if the current entry is still this element (guards against
        // a remount registering the new node before the old one unregisters).
        if (slotsRef.current.get(id) === el) {
          slotsRef.current.delete(id);
          bump();
        }
      },
      requestSync: bump,
    }),
    [bump],
  );

  return (
    <PoolContext.Provider value={registry}>
      <div ref={containerRef} className="relative h-full w-full">
        {children}
        <TerminalPoolLayer
          containerRef={containerRef}
          slotsRef={slotsRef}
          version={version}
        />
      </div>
    </PoolContext.Provider>
  );
}

interface PoolLayerProps {
  containerRef: React.RefObject<HTMLDivElement | null>;
  slotsRef: React.RefObject<Map<TerminalId, HTMLElement>>;
  /** Bumps whenever the placeholder set changes; re-runs the position sync. */
  version: number;
}

/**
 * The absolute overlay holding one pooled <TerminalView> per live terminal. The
 * wrappers are keyed by terminal id and never unmount while the terminal exists,
 * so xterm stays mounted/attached across tab moves and reorders. A layout-sync
 * effect positions each wrapper over its placeholder; wrappers with no visible
 * placeholder are hidden and parked.
 */
function TerminalPoolLayer({ containerRef, slotsRef, version }: PoolLayerProps) {
  // Every terminal id that has a tile somewhere (across ALL tabs), de-duped and
  // in a stable order so React keeps each wrapper mounted. We render the union of
  // tab orders rather than the live `terminals` map so a wrapper exists exactly
  // for tiles the user placed (and survives tab switches).
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);

  const poolIds = useMemo(() => {
    const seen = new Set<TerminalId>();
    const ids: TerminalId[] = [];
    for (const t of tabs) {
      for (const id of t.order) {
        if (!seen.has(id)) {
          seen.add(id);
          ids.push(id);
        }
      }
    }
    return ids;
  }, [tabs]);

  // Stable wrapper element refs so the sync can write inline position styles
  // imperatively (no React re-render per pointer-move while resizing/dragging).
  const wrapRefs = useRef<Map<TerminalId, HTMLDivElement>>(new Map());

  // Derive, per render, which tab each terminal lives in — so the sync knows
  // whether its placeholder belongs to the *active* (displayed) tab. A terminal
  // on an inactive tab has a placeholder in the DOM (every tab stays mounted),
  // but that placeholder has a zero-area rect (its tab is display:none), so we
  // additionally gate on tab activity to avoid stacking it at 0,0.
  const tabOfId = useMemo(() => {
    const m = new Map<TerminalId, string>();
    for (const t of tabs) for (const id of t.order) m.set(id, t.id);
    return m;
  }, [tabs]);

  // Position every pooled terminal over its placeholder. Runs after layout
  // (useLayoutEffect) so we read settled rects and paint with no flash.
  const sync = useCallback(() => {
    const container = containerRef.current;
    if (!container) return;
    const base = container.getBoundingClientRect();
    const slots = slotsRef.current;
    if (!slots) return;

    for (const id of wrapRefs.current.keys()) {
      const wrap = wrapRefs.current.get(id);
      if (!wrap) continue;
      const slot = slots.get(id);
      const onActiveTab = tabOfId.get(id) === activeTabId;
      const rect = slot?.getBoundingClientRect();
      // Show only when the terminal's tile is on the active tab and its
      // placeholder has a real (non-zero) box. Otherwise park it hidden so the
      // xterm stays mounted/attached in the background.
      if (slot && onActiveTab && rect && rect.width > 0 && rect.height > 0) {
        wrap.style.visibility = "visible";
        wrap.style.pointerEvents = "";
        wrap.style.transform = `translate(${rect.left - base.left}px, ${
          rect.top - base.top
        }px)`;
        wrap.style.width = `${rect.width}px`;
        wrap.style.height = `${rect.height}px`;
      } else {
        // Keep mounted but invisible + inert. Park at the last known size so a
        // hidden tab's xterm isn't forced to a 0x0 (which would refit to 0 cols).
        wrap.style.visibility = "hidden";
        wrap.style.pointerEvents = "none";
        if (rect && rect.width > 0 && rect.height > 0) {
          wrap.style.width = `${rect.width}px`;
          wrap.style.height = `${rect.height}px`;
        }
        // Leave the offscreen transform in place (set on creation) so a parked
        // terminal never overlaps the active grid.
        wrap.style.transform = "translate(-100000px, 0px)";
      }
    }
  }, [containerRef, slotsRef, tabOfId, activeTabId]);

  // Re-sync on every dependency that can move a placeholder: the placeholder set
  // (version), the active tab, and the tabs array (order/sizes changes all
  // produce a new `tabs` reference via the store). useLayoutEffect lands the
  // position before paint so a tab switch / move shows no transient mis-place.
  useLayoutEffect(() => {
    sync();
  }, [sync, version, tabs, activeTabId]);

  // Keep terminals glued to their placeholders as the window/container resizes
  // or the flex grid reflows (gutter drags resize cells without changing `tabs`
  // until pointer-up). A ResizeObserver on the container catches container-size
  // changes; we also observe each registered placeholder so a single cell's
  // resize (e.g. a column gutter drag) repositions just-in-time. rAF-coalesced
  // so a burst of observer callbacks does one measure per frame.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let raf = 0;
    const schedule = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        sync();
      });
    };
    const ro = new ResizeObserver(schedule);
    ro.observe(container);
    const slots = slotsRef.current;
    if (slots) for (const el of slots.values()) ro.observe(el);
    window.addEventListener("resize", schedule);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener("resize", schedule);
    };
    // Re-establish observers when the placeholder set changes (version) so newly
    // mounted cells are observed and removed ones are dropped.
  }, [containerRef, slotsRef, sync, version]);

  return (
    <div
      // Pool overlay: spans the canvas body. It paints ABOVE the grid cells'
      // backgrounds (those are non-positioned, so this positioned layer wins) but
      // BELOW the gutters/intersection handles, which carry z-10/z-20 — so their
      // full negative-margin hit zones still win the pointer at cell edges. `z-0`
      // (a positioned auto-context layer) is what lands the overlay between the
      // two. The container is click-through (pointer-events:none) so headers and
      // gutters stay grabbable; each pooled terminal re-enables pointer events on
      // itself so it's interactive.
      className="pointer-events-none absolute inset-0 z-0"
      aria-hidden={false}
    >
      {poolIds.map((id) => (
        <div
          key={id}
          // data-th-pool-tile lets index.css make the wrapper pointer-inert
          // during a tile drag, so elementFromPoint falls through to the grid
          // placeholder (data-tile-id) underneath for drop resolution.
          data-th-pool-tile={id}
          ref={(el) => {
            if (el) {
              wrapRefs.current.set(id, el);
              // Park offscreen until the first sync positions it (avoids a 0,0
              // flash on mount before useLayoutEffect runs).
              el.style.transform = "translate(-100000px, 0px)";
            } else {
              wrapRefs.current.delete(id);
            }
          }}
          className="pointer-events-auto absolute left-0 top-0 overflow-hidden"
          style={{ visibility: "hidden" }}
        >
          {/* Rendered ONCE, here, for this terminal's whole lifetime. Stable key
              + stable parent => xterm is never remounted on a tab move/reorder.
              visible is always true (pool keeps every terminal attached). */}
          <TerminalView terminalId={id} visible={true} />
        </div>
      ))}
    </div>
  );
}
