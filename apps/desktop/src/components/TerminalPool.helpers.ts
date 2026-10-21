import {
  createContext,
  useCallback,
  useContext,
  useRef
} from "react";
import type { TerminalId } from "../ipc/types";

// ---------------------------------------------------------------------------
// Placeholder registry (context). Each Tile registers the empty body box it
// renders, keyed by terminal id; the pool reads the registry to know where to
// position each pooled terminal. A monotonic `version` bumps whenever the set of
// placeholders changes (mount/unmount) so the pool re-syncs immediately.
// ---------------------------------------------------------------------------

export interface PoolRegistry {
  /** Register (or replace) the body placeholder element for a terminal id. */
  register: (id: TerminalId, el: HTMLElement) => void;
  /** Remove a terminal id's placeholder (on tile unmount). */
  unregister: (id: TerminalId, el: HTMLElement) => void;
  /** Ask the pool to re-measure now (e.g. after a layout-affecting change). */
  requestSync: () => void;
}

export const PoolContext = createContext<PoolRegistry | null>(null);

/**
 * Register this element as terminal `id`'s body placeholder for the lifetime of
 * the calling component. The pool positions `id`'s pooled terminal over it.
 * No-op outside a <TerminalPoolProvider> (so Tile still works standalone).
 */
// ---------------------------------------------------------------------------
// External imperative re-sync (captain-overlay). The overlay's drag/resize
// gestures move its slot placeholder WITHOUT any store/layout change the pool
// would notice (ResizeObserver only fires on SIZE changes, and we deliberately
// avoid a React version-bump per pointermove). This module-level hook lets the
// gesture call straight into the live layer's imperative sync(), which measures
// and repositions without a render. Null outside a mounted pool layer.
// ---------------------------------------------------------------------------

export const poolSync: { current: ((trigger: string) => void) | null } = { current: null };

/** Imperatively re-measure + reposition every pooled terminal now. No-op when
 *  no pool layer is mounted. Safe to call at pointer-move rate (the caller
 *  should still rAF-coalesce). */
export function requestPoolSync(trigger: string): void {
  poolSync.current?.(trigger);
}

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
