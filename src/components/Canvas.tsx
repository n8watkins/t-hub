// The canvas renders the active workspace tab as a responsive auto-grid of
// terminal tiles (PRD §5.2 tabs, §5.3 layout):
//   - On mount: listTerminals() seeds the store; onState() keeps tile chrome live.
//   - The workspace tab strip lives in the top bar (Titlebar) now, not here.
//   - Each tab is a deterministic near-square grid sized from its tile count.
//   - Spawn (+ button, empty-state button, Ctrl/Cmd+T) inserts after the focused
//     tile in the active tab; Ctrl/Cmd+W detaches the focused tile.
//   - Manual mode: draggable gutters between rows/columns adjust their flex
//     ratios, persisted per tab (PRD §5.3 resize). Each gutter has a wide,
//     invisible hit zone with a thin visible indicator for easy grabbing.
//   - Shell v2 tab persistence: EVERY tab stays mounted at all times. The active
//     tab is shown and inactive tabs are hidden with CSS `display:none`.
//   - #20 terminal pool: each terminal's xterm is rendered ONCE in a persistent,
//     never-reparented overlay (TerminalPoolProvider) and positioned over its
//     tile's empty body placeholder. Switching tabs, reordering, resizing, or
//     moving a tile to another tab only REPOSITIONS the pooled terminal — it is
//     never unmounted/reattached, so there's no reload/flash. Terminal.tsx's own
//     ResizeObserver refits a terminal when its positioned box changes size.
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useWorkspace } from "../store/workspace";
import type { WorkspaceTab } from "../store/workspace";
import {
  spawnTerminal,
  listTerminals,
  closeTerminal,
  onState,
} from "../ipc/client";
import { Tile } from "./Tile";
import { TerminalPoolProvider } from "./TerminalPool";
import type { TerminalId } from "../ipc/types";

/**
 * Split `ids` into balanced rows that completely fill the canvas — no empty
 * cells. Columns target a near-square (cols = ceil(sqrt(n))); the tiles are then
 * spread as evenly as possible across the rows, so a short last row's tiles just
 * grow wider instead of leaving a gap.
 */
function splitRows<T>(ids: T[]): T[][] {
  const n = ids.length;
  if (n === 0) return [];
  const cols = Math.ceil(Math.sqrt(n));
  const rows = Math.ceil(n / cols);
  const base = Math.floor(n / rows);
  const extra = n % rows; // the first `extra` rows get one additional tile
  const out: T[][] = [];
  let i = 0;
  for (let r = 0; r < rows; r++) {
    const count = base + (r < extra ? 1 : 0);
    out.push(ids.slice(i, i + count));
    i += count;
  }
  return out;
}

export interface CanvasProps {
  /** Toggle the 0.5 supervision sidebar (Ctrl/Cmd+B). Optional so the 0.1
   *  nucleus canvas still works standalone. */
  onToggleSidebar?: () => void;
}

export function Canvas({ onToggleSidebar }: CanvasProps = {}) {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const focusedId = useWorkspace((s) => s.focusedId);
  const setTerminals = useWorkspace((s) => s.setTerminals);
  const addAfterFocused = useWorkspace((s) => s.addAfterFocused);
  const remove = useWorkspace((s) => s.remove);
  const setFocus = useWorkspace((s) => s.setFocus);
  const updateState = useWorkspace((s) => s.updateState);
  const cycleTab = useWorkspace((s) => s.cycleTab);
  const setActiveTabByIndex = useWorkspace((s) => s.setActiveTabByIndex);
  const zoomIn = useWorkspace((s) => s.zoomIn);
  const zoomOut = useWorkspace((s) => s.zoomOut);
  const zoomReset = useWorkspace((s) => s.zoomReset);

  // Seed the live terminal set and keep lifecycle state in sync with the backend.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;

    void listTerminals()
      .then(setTerminals)
      .catch((err) => console.error("listTerminals failed", err));

    void onState((e) => updateState(e.id, e.state))
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error("onState subscribe failed", err));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [setTerminals, updateState]);

  const spawn = useCallback(async () => {
    try {
      const info = await spawnTerminal({});
      addAfterFocused(info);
    } catch (err) {
      console.error("spawnTerminal failed", err);
    }
  }, [addAfterFocused]);

  const closeFocused = useCallback(() => {
    const id = useWorkspace.getState().focusedId;
    if (!id) return;
    void closeTerminal(id).catch((err) =>
      console.error("closeTerminal failed", err),
    );
    remove(id);
  }, [remove]);

  const close = useCallback(
    (id: string) => {
      void closeTerminal(id).catch((err) =>
        console.error("closeTerminal failed", err),
      );
      remove(id);
    },
    [remove],
  );

  // Global keybindings: Ctrl/Cmd+T = new terminal, Ctrl/Cmd+W = close focused,
  // Ctrl/Cmd+B = toggle the supervision sidebar, Ctrl/Cmd+Tab = cycle tabs
  // (Shift reverses), Ctrl/Cmd+1..9 = jump to the tab at that index.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      // Ctrl/Cmd+Tab cycles workspace tabs (Shift => previous).
      if (e.key === "Tab" && !e.altKey) {
        e.preventDefault();
        cycleTab(e.shiftKey ? -1 : 1);
        return;
      }
      if (e.altKey) return;
      // Ctrl/Cmd+1..9 jumps straight to that tab (1-based -> 0-based index).
      if (e.key >= "1" && e.key <= "9") {
        e.preventDefault();
        setActiveTabByIndex(Number(e.key) - 1);
        return;
      }
      const key = e.key.toLowerCase();
      if (key === "t") {
        e.preventDefault();
        void spawn();
      } else if (key === "w") {
        e.preventDefault();
        closeFocused();
      } else if (key === "=" || key === "+") {
        e.preventDefault();
        zoomIn();
      } else if (key === "-" || key === "_") {
        e.preventDefault();
        zoomOut();
      } else if (key === "0") {
        e.preventDefault();
        zoomReset();
      } else if (key === "b" && onToggleSidebar) {
        e.preventDefault();
        onToggleSidebar();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    spawn,
    closeFocused,
    cycleTab,
    setActiveTabByIndex,
    zoomIn,
    zoomOut,
    zoomReset,
    onToggleSidebar,
  ]);

  return (
    <div
      className="relative flex h-full w-full flex-col"
      style={{ backgroundColor: "var(--th-app-bg)" }}
    >
      <div className="relative min-h-0 flex-1">
        {/* Shell v2 + #20 pool: every tab stays mounted; only the active tab is
            displayed (inactive tabs are display:none). The tile bodies are EMPTY
            placeholders — each terminal's xterm is rendered once in the persistent
            pool overlay (TerminalPoolProvider) and positioned over its current
            placeholder, so a tab switch / move never unmounts or reloads it. */}
        <TerminalPoolProvider>
          {tabs.map((tab) => {
            const active = tab.id === activeTabId;
            return (
              <div
                key={tab.id}
                className="absolute inset-0"
                style={{ display: active ? undefined : "none" }}
                aria-hidden={!active}
              >
                {tab.order.length === 0 ? (
                  <EmptyTab onSpawn={() => void spawn()} />
                ) : (
                  <TabGrid
                    tab={tab}
                    active={active}
                    focusedId={focusedId}
                    onFocus={setFocus}
                    onClose={close}
                  />
                )}
              </div>
            );
          })}
        </TerminalPoolProvider>
      </div>

      {/* Persistent affordance to add more terminals to the active tab. */}
      <button
        type="button"
        onClick={() => void spawn()}
        title="New terminal (Ctrl/Cmd+T)"
        aria-label="New terminal"
        // Themed FAB: tile-surface bg + themed border/text; the hover border
        // picks up the accent so the primary affordance follows the theme.
        className="th-accent-hover absolute bottom-3 right-3 z-30 flex h-9 w-9 cursor-pointer items-center justify-center rounded-full border text-lg leading-none shadow-lg"
        style={{
          backgroundColor: "var(--th-tile-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
        }}
      >
        +
      </button>
    </div>
  );
}

/** Centered call-to-action shown when the active tab has no tiles. */
function EmptyTab({ onSpawn }: { onSpawn: () => void }) {
  return (
    <div
      className="flex h-full w-full items-center justify-center"
      style={{ backgroundColor: "var(--th-app-bg)" }}
    >
      <button
        type="button"
        onClick={onSpawn}
        className="rounded-md border border-neutral-700 bg-neutral-900 px-5 py-3 text-base text-neutral-200 hover:border-emerald-600 hover:text-white"
      >
        ＋ New terminal
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Tab grid + manual resize (PRD §5.3). Tiles are laid out in balanced rows; a
// draggable gutter between each adjacent pair of rows (and of columns within a
// row) adjusts their flex-grow weights, which are persisted on the tab.
// ---------------------------------------------------------------------------

/**
 * Smallest flex weight a row/column may shrink to while dragging a gutter, and a
 * hard pixel floor so a tile can never be dragged uselessly small regardless of
 * how many tiles share the axis. The effective minimum a side may shrink to is
 * the LARGER of MIN_FLEX (a relative floor) and the weight equivalent of
 * MIN_TILE_PX (an absolute floor, computed per-drag from the measured extent).
 */
const MIN_FLEX = 0.35;
/** Absolute minimum tile edge in CSS px enforced during a gutter/cross drag. */
const MIN_TILE_PX = 160;

/**
 * Per-drag minimum weight for ONE side of a two-element split. `extentPx` is the
 * pixel span the pair's weights are distributed over and `pairWeight` is the
 * pair's combined weight (so weight/pairWeight is the side's fraction of the
 * span). Returns the larger of MIN_FLEX and the weight that maps to MIN_TILE_PX,
 * but never more than just under the whole pair (so the other side keeps a sliver
 * even if the span itself is below 2*MIN_TILE_PX).
 */
function minSideWeight(extentPx: number, pairWeight: number): number {
  let floor = MIN_FLEX;
  if (extentPx > 0 && pairWeight > 0) {
    const pxFloor = (MIN_TILE_PX / extentPx) * pairWeight;
    if (pxFloor > floor) floor = pxFloor;
  }
  // Keep both sides representable: never let one side's floor exceed the pair.
  return Math.min(floor, pairWeight * 0.49);
}

interface TabGridProps {
  tab: WorkspaceTab;
  active: boolean;
  focusedId: TerminalId | null;
  onFocus: (id: TerminalId) => void;
  onClose: (id: TerminalId) => void;
}

function TabGrid({ tab, active, focusedId, onFocus, onClose }: TabGridProps) {
  const layout = splitRows(tab.order);
  const setTabSizes = useWorkspace((s) => s.setTabSizes);
  const containerRef = useRef<HTMLDivElement | null>(null);

  // Local, editable copy of the flex weights so dragging is smooth (we only
  // write through to the store at pointer-up). Re-derived whenever the tab's
  // shape (row/col counts) or persisted sizes change.
  const rowCount = layout.length;
  const colKey = layout.map((r) => r.length).join(",");
  const [rows, setRows] = useState<number[]>(() =>
    normalize(tab.sizes?.rows, rowCount),
  );
  const [cols, setCols] = useState<number[][]>(() =>
    layout.map((row, r) => normalize(tab.sizes?.cols?.[r], row.length)),
  );
  // Refs mirror the live weights so the drag handlers (registered once) and the
  // pointer-up persist can read the latest values without stale closures.
  const rowsRef = useRef(rows);
  const colsRef = useRef(cols);
  rowsRef.current = rows;
  colsRef.current = cols;

  // Resync when the grid shape changes (tiles added/removed/moved) or the
  // persisted sizes change out from under us.
  useLayoutEffect(() => {
    setRows(normalize(tab.sizes?.rows, rowCount));
    setCols(layout.map((row, r) => normalize(tab.sizes?.cols?.[r], row.length)));
    // colKey + rowCount capture the shape; tab.sizes captures persisted change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rowCount, colKey, tab.sizes]);

  // --- Gutter drag (pointer-based) ---
  // A single-axis gutter sits between elements i and i+1; dragging it trades
  // weight between them proportionally to the pointer delta over the container's
  // extent. The "cross" handle sits at an internal crosspoint where a row seam
  // meets a column seam aligned across the two adjacent rows; dragging it drives
  // BOTH a vertical row split (rows[r]/rows[r+1]) and a horizontal column split
  // at index c in BOTH rows r and r+1 at once, kept in sync so all four
  // surrounding tiles resize together. The state is a discriminated union so the
  // cross variant carries its own two-axis geometry without polluting the
  // single-axis fields.
  type DragState =
    | {
        axis: "row";
        i: number; // top row index of the pair
        startPos: number; // clientY at pointer-down
        extentPx: number; // grid height
        aStart: number; // rows[i]
        bStart: number; // rows[i+1]
      }
    | {
        axis: "col";
        rowIdx: number; // which row's columns
        i: number; // left column index of the pair
        startPos: number; // clientX at pointer-down
        extentPx: number; // row width
        aStart: number; // cols[rowIdx][i]
        bStart: number; // cols[rowIdx][i+1]
      }
    | {
        axis: "cross";
        r: number; // top row index (seam is between r and r+1)
        startX: number;
        startY: number;
        rowExtentPx: number; // grid height (vertical extent)
        colExtentPx: number; // row width (horizontal extent)
        rowAStart: number; // rows[r]
        rowBStart: number; // rows[r+1]
        // Each row's column split at the junction is OPTIONAL: a 4-tile junction
        // has a real boundary in BOTH rows; a 3-tile junction has one in only the
        // row that has a seam there (the other row's cell spans across the seam,
        // so dragging horizontally must not disturb it). `top`/`bot` are null when
        // that row spans the junction.
        top: CrossAxis | null; // cols[r]   split, if the top row has one here
        bot: CrossAxis | null; // cols[r+1] split, if the bottom row has one here
      };
  const dragRef = useRef<DragState | null>(null);

  const onPointerMove = useCallback((e: PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;

    if (d.axis === "cross") {
      // --- Vertical: trade rows[r]/rows[r+1] (same math as a row gutter). ---
      if (d.rowExtentPx > 0) {
        const rowTotal = d.rowAStart + d.rowBStart;
        const rowMin = minSideWeight(d.rowExtentPx, rowTotal);
        let rDelta = ((e.clientY - d.startY) / d.rowExtentPx) * rowTotal;
        rDelta = Math.max(
          -(d.rowAStart - rowMin),
          Math.min(d.rowBStart - rowMin, rDelta),
        );
        const next = rowsRef.current.slice();
        next[d.r] = d.rowAStart + rDelta;
        next[d.r + 1] = d.rowBStart - rDelta;
        rowsRef.current = next;
        setRows(next);
      }

      // --- Horizontal: slide the column seam to the same PIXEL position in
      // whichever adjacent row(s) own a seam here. Both rows fill the same row
      // width, so a target x maps to the same pixel offset for each; a 4-tile
      // junction drives both (keeping them visually aligned as they were at
      // pointer-down), a 3-tile junction drives only the side that has a seam
      // (the spanning side is left untouched). See `slide` below for the per-row
      // math. ---
      if (d.colExtentPx > 0 && (d.top || d.bot)) {
        const dxFrac = (e.clientX - d.startX) / d.colExtentPx; // delta as fraction of row width
        const next = colsRef.current.map((row) => row.slice());

        // Slide ONE row's column pair. Each row's pair occupies pairSum/rowSum of
        // the full row width, so a row-width-fraction delta maps into a delta
        // within the pair's own span (dxFrac / pairFrac). The pair's combined
        // weight (and thus its outer pixel edges) is fixed, so we only redivide
        // it. Each cell is floored at the larger of MIN_FLEX and MIN_TILE_PX
        // (converted to a fraction of the pair span). A 3-tile junction calls
        // this for just the side that has a seam; the spanning side is untouched.
        const slide = (rowIdx: number, ax: CrossAxis) => {
          const pairSum = ax.aStart + ax.bStart;
          if (pairSum <= 0 || ax.rowSum <= 0) return;
          const pairFrac = pairSum / ax.rowSum; // pair's share of the row width
          const pairSpanPx = pairFrac * d.colExtentPx;
          let f = ax.aStart / pairSum + (pairFrac > 0 ? dxFrac / pairFrac : 0);
          const minF = minSideWeight(pairSpanPx, pairSum) / pairSum;
          f = Math.max(minF, Math.min(1 - minF, f));
          next[rowIdx][ax.c] = f * pairSum;
          next[rowIdx][ax.c + 1] = (1 - f) * pairSum;
        };

        if (d.top) slide(d.r, d.top);
        if (d.bot) slide(d.r + 1, d.bot);

        colsRef.current = next;
        setCols(next);
      }
      return;
    }

    if (d.extentPx <= 0) return;
    const pos = d.axis === "row" ? e.clientY : e.clientX;
    const total = d.aStart + d.bStart;
    // Convert px delta to weight delta (weights here sum to `total`).
    let delta = ((pos - d.startPos) / d.extentPx) * total;
    // Clamp so neither side drops below the effective minimum (the larger of
    // MIN_FLEX and the MIN_TILE_PX pixel floor for this pair's span).
    const minSide = minSideWeight(d.extentPx, total);
    const maxDelta = d.bStart - minSide;
    const minDelta = -(d.aStart - minSide);
    delta = Math.max(minDelta, Math.min(maxDelta, delta));
    const a = d.aStart + delta;
    const b = d.bStart - delta;

    if (d.axis === "row") {
      const next = rowsRef.current.slice();
      next[d.i] = a;
      next[d.i + 1] = b;
      rowsRef.current = next;
      setRows(next);
    } else {
      const next = colsRef.current.map((row) => row.slice());
      next[d.rowIdx][d.i] = a;
      next[d.rowIdx][d.i + 1] = b;
      colsRef.current = next;
      setCols(next);
    }
  }, []);

  const endDrag = useCallback(() => {
    if (!dragRef.current) return;
    dragRef.current = null;
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", endDrag);
    document.body.style.removeProperty("cursor");
    document.body.style.removeProperty("user-select");
    // Persist the freshly dragged weights for this tab.
    setTabSizes(tab.id, {
      rows: rowsRef.current.slice(),
      cols: colsRef.current.map((row) => row.slice()),
    });
  }, [onPointerMove, setTabSizes, tab.id]);

  const beginRowDrag = (i: number, e: ReactPointerEvent) => {
    const el = containerRef.current;
    if (!el) return;
    e.preventDefault();
    dragRef.current = {
      axis: "row",
      i,
      startPos: e.clientY,
      extentPx: el.getBoundingClientRect().height,
      aStart: rows[i] ?? 1,
      bStart: rows[i + 1] ?? 1,
    };
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", endDrag);
  };

  const beginColDrag = (rowIdx: number, i: number, e: ReactPointerEvent) => {
    // Walk up to the row flex container (the gutter's immediate parent), whose
    // measured width is the extent the column weights are distributed over.
    const rowEl = (e.currentTarget as HTMLElement).parentElement;
    if (!rowEl) return;
    e.preventDefault();
    dragRef.current = {
      axis: "col",
      rowIdx,
      i,
      startPos: e.clientX,
      extentPx: rowEl.getBoundingClientRect().width,
      aStart: cols[rowIdx]?.[i] ?? 1,
      bStart: cols[rowIdx]?.[i + 1] ?? 1,
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", endDrag);
  };

  // Intersection drag: r = top row of the seam. `topC`/`botC` are each that
  // row's left column index of the dragged pair, or null when that row spans the
  // seam (a 3-tile junction). Vertical extent is the grid container's height;
  // horizontal extent is the width of the row immediately above the seam (the
  // gutter's previous sibling), which both adjacent rows share.
  const beginCrossDrag = (
    r: number,
    topC: number | null,
    botC: number | null,
    e: ReactPointerEvent,
  ) => {
    const grid = containerRef.current;
    if (!grid) return;
    const rowAbove = (e.currentTarget as HTMLElement).closest(
      "[data-row-gutter]",
    )?.previousElementSibling as HTMLElement | null;
    e.preventDefault();
    e.stopPropagation(); // don't also start the RowGutter's single-axis drag
    const top = colsRef.current[r] ?? [];
    const bot = colsRef.current[r + 1] ?? [];
    const sum = (arr: number[]) => arr.reduce((a, b) => a + b, 0) || 1;
    const axis = (row: number[], c: number | null): CrossAxis | null =>
      c === null ? null : { c, rowSum: sum(row), aStart: row[c] ?? 1, bStart: row[c + 1] ?? 1 };
    dragRef.current = {
      axis: "cross",
      r,
      startX: e.clientX,
      startY: e.clientY,
      rowExtentPx: grid.getBoundingClientRect().height,
      colExtentPx: (rowAbove ?? grid).getBoundingClientRect().width,
      rowAStart: rowsRef.current[r] ?? 1,
      rowBStart: rowsRef.current[r + 1] ?? 1,
      top: axis(top, topC),
      bot: axis(bot, botC),
    };
    document.body.style.cursor = "nwse-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", endDrag);
  };

  // Detach window listeners if we unmount mid-drag.
  useEffect(() => {
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", endDrag);
    };
  }, [onPointerMove, endDrag]);

  // Build a flat, interleaved child list (row, gutter, row, …) with NO
  // display:contents wrappers, so every gutter's parentElement is a real flex
  // container with a measurable box (needed for the drag math).
  return (
    <div
      ref={containerRef}
      className="flex h-full w-full flex-col"
      style={{ gap: "var(--th-grid-gap)", padding: "var(--th-grid-gap)" }}
    >
      {layout.flatMap((row, r) => {
        const rowEl = (
          <div
            key={`row-${r}`}
            className="flex min-h-0"
            style={{
              flexGrow: rows[r] ?? 1,
              flexBasis: 0,
              gap: "var(--th-grid-gap)",
            }}
          >
            {row.flatMap((id, c) => {
              const cell = (
                <div
                  key={id}
                  className="min-h-0 min-w-0"
                  style={{ flexGrow: cols[r]?.[c] ?? 1, flexBasis: 0 }}
                >
                  <Tile
                    terminalId={id}
                    focused={active && id === focusedId}
                    // #20: the xterm body lives in the persistent pool overlay,
                    // not in the tile — the tile renders header + placeholder.
                    onFocus={() => onFocus(id)}
                    onClose={() => onClose(id)}
                  />
                </div>
              );
              if (c === 0) return [cell];
              // Column gutter: a wide (8px), invisible-but-grabbable hit zone
              // straddling the seam, with a thin centered indicator that
              // brightens on hover. Negative margins keep the visible gap at 1px
              // while the hit zone overhangs both neighbors for easy grabbing.
              const gutter = (
                <ColGutter
                  key={`cg-${r}-${c}`}
                  onPointerDown={(e) => beginColDrag(r, c - 1, e)}
                />
              );
              return [gutter, cell];
            })}
          </div>
        );
        if (r === 0) return [rowEl];
        // Row gutter: same wide invisible hit zone, horizontal orientation. This
        // seam lies between row r-1 (above) and row r (below); intersection
        // handles sit on every column boundary those two rows share so the
        // crosspoint resizes all 4 adjacent tiles at once.
        const crossPoints = alignedCrossPoints(
          cols[r - 1] ?? [],
          cols[r] ?? [],
        );
        const gutter = (
          <RowGutter
            key={`rg-${r}`}
            onPointerDown={(e) => beginRowDrag(r - 1, e)}
            crossPoints={crossPoints}
            onCrossPointerDown={(topC, botC, e) =>
              beginCrossDrag(r - 1, topC, botC, e)
            }
          />
        );
        return [gutter, rowEl];
      })}
    </div>
  );
}

/**
 * Column resize gutter. The outer element is a wide (8px) transparent hit zone
 * with `col-resize` cursor; negative horizontal margins let it overhang its
 * neighbors so the actual visible gap stays ~1px. The inner 1px line is the
 * visible indicator: faint by default, emerald on hover.
 */
function ColGutter({
  onPointerDown,
}: {
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={onPointerDown}
      // 14px-wide hit zone (was 8px) for an easier grab; negative margins keep
      // the visible gap ~1px while the zone overhangs both neighbors further.
      className="group relative z-10 -mx-[6.5px] w-[14px] shrink-0 cursor-col-resize"
    >
      <div className="th-gutter-line absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-neutral-700/60 transition-colors" />
    </div>
  );
}

/**
 * Row resize gutter — the horizontal twin of ColGutter (8px tall hit zone,
 * `row-resize` cursor, negative vertical margins, 1px visible indicator).
 *
 * It also hosts the intersection handles: for each column boundary that aligns
 * across the two rows this seam separates, a small square is absolutely centered
 * on the crosspoint (at the boundary's fraction of the row width). The square
 * sits above the row line (`z-20` vs the line's gutter `z-10`) with a wider hit
 * zone so it wins the pointer at the exact 4-tile junction, and drives a
 * two-axis (`nwse-resize`) drag while the surrounding gutter still handles the
 * rest of the seam.
 */
function RowGutter({
  onPointerDown,
  crossPoints,
  onCrossPointerDown,
}: {
  onPointerDown: (e: ReactPointerEvent) => void;
  crossPoints?: CrossPoint[];
  onCrossPointerDown?: (
    topC: number | null,
    botC: number | null,
    e: ReactPointerEvent,
  ) => void;
}) {
  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      data-row-gutter=""
      onPointerDown={onPointerDown}
      // 14px-tall hit zone (was 8px) to match the wider column gutter.
      className="group relative z-10 -my-[6.5px] h-[14px] shrink-0 cursor-row-resize"
    >
      <div className="th-gutter-line absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-neutral-700/60 transition-colors" />
      {crossPoints?.map((cp) => (
        <IntersectionHandle
          key={cp.key}
          fraction={cp.fraction}
          onPointerDown={(e) => onCrossPointerDown?.(cp.topC, cp.botC, e)}
        />
      ))}
    </div>
  );
}

/**
 * The draggable crosspoint where 4 tiles meet. A small square, centered on the
 * column seam (`left: fraction`) and on the row seam (the gutter's mid-line). It
 * stays visually subtle by default and brightens to the accent on hover, mirror-
 * ing the `.th-gutter-line` feel; `cursor: nwse-resize` signals the two-axis
 * resize. The transparent hit box is larger than the visible dot for easy grab.
 */
function IntersectionHandle({
  fraction,
  onPointerDown,
}: {
  fraction: number;
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  return (
    <div
      role="separator"
      aria-label="Resize rows and columns"
      onPointerDown={onPointerDown}
      // 20px square transparent hit zone (was 12px) centered on the crosspoint,
      // above the single-axis gutter lines so it wins the pointer at the
      // junction. The visible dot stays small; only the grab target grows.
      className="group/xh absolute top-1/2 z-20 h-5 w-5 -translate-x-1/2 -translate-y-1/2 cursor-nwse-resize"
      style={{ left: `${fraction * 100}%` }}
    >
      <div className="absolute left-1/2 top-1/2 h-[6px] w-[6px] -translate-x-1/2 -translate-y-1/2 rounded-[1px] bg-neutral-600/70 transition-colors group-hover/xh:bg-[var(--th-accent)]" />
    </div>
  );
}

/**
 * A junction between two vertically adjacent rows where at least one row has an
 * internal column seam. Drives a 2-axis (row split + column split) handle. When
 * BOTH rows have a seam at the same fraction it's a 4-tile junction; when only
 * one does it's a 3-tile junction (the other row's cell spans the seam, and the
 * handle only drags the column split on the side that has one).
 */
interface CrossPoint {
  /** Horizontal position of the seam as a fraction (0..1) of the row width. */
  fraction: number;
  /** Left column index of the top row's pair at this seam, or null if it spans. */
  topC: number | null;
  /** Left column index of the bottom row's pair at this seam, or null if it spans. */
  botC: number | null;
  /** Stable identity for React keying (one handle per fraction). */
  key: string;
}

/** One row's column-split geometry captured at a cross-drag's pointer-down. */
interface CrossAxis {
  c: number; // left column index of the dragged pair
  rowSum: number; // sum of the whole row's weights
  aStart: number; // cols[row][c]
  bStart: number; // cols[row][c+1]
}

/**
 * Internal column boundaries between two vertically adjacent rows that warrant a
 * 2-axis (row-split + column-split) handle. EVERY internal boundary in EITHER
 * row qualifies, because the other row always either has an aligned boundary
 * there (a 4-tile junction — drag both column splits) or a cell that spans it (a
 * 3-tile junction — drag only the row that has the seam). The returned
 * `topC`/`botC` carry each row's left column index of the pair to drag at that
 * fraction, or null when that row spans the seam. We collect each row's boundary
 * fractions, then merge near-coincident ones (within EPS) into a single 4-tile
 * handle so we don't stack two handles on top of each other.
 */
function alignedCrossPoints(top: number[], bot: number[]): CrossPoint[] {
  // Internal boundaries of one row as (fraction, leftColIndex) pairs.
  const boundaries = (row: number[]): { f: number; c: number }[] => {
    const sum = row.reduce((a, b) => a + b, 0);
    if (sum <= 0 || row.length < 2) return [];
    const out: { f: number; c: number }[] = [];
    let cum = 0;
    for (let c = 0; c < row.length - 1; c++) {
      cum += row[c];
      out.push({ f: cum / sum, c });
    }
    return out;
  };

  const topB = boundaries(top);
  const botB = boundaries(bot);
  // Tolerance treats a freshly-even uniform grid (and small float drift) as
  // aligned, but a deliberately dragged split in only one row reads as
  // misaligned (a 3-tile junction).
  const EPS = 0.02;

  const out: CrossPoint[] = [];
  let ti = 0;
  let bi = 0;
  // Merge-walk both rows' boundary lists in fraction order. Coincident
  // boundaries (|df| <= EPS) fuse into one 4-tile handle; a lone boundary in one
  // row becomes a 3-tile handle (other side null).
  while (ti < topB.length || bi < botB.length) {
    const t = ti < topB.length ? topB[ti] : null;
    const b = bi < botB.length ? botB[bi] : null;
    if (t && b && Math.abs(t.f - b.f) <= EPS) {
      const f = (t.f + b.f) / 2;
      out.push({ fraction: f, topC: t.c, botC: b.c, key: `x-${f.toFixed(4)}` });
      ti += 1;
      bi += 1;
    } else if (b === null || (t !== null && t.f < b.f)) {
      // Top-only boundary: 3-tile junction, bottom row spans it.
      out.push({ fraction: t!.f, topC: t!.c, botC: null, key: `t-${t!.f.toFixed(4)}` });
      ti += 1;
    } else {
      // Bottom-only boundary: 3-tile junction, top row spans it.
      out.push({ fraction: b!.f, topC: null, botC: b!.c, key: `b-${b!.f.toFixed(4)}` });
      bi += 1;
    }
  }
  return out;
}

/**
 * Coerce a persisted/absent weight array into a clean array of length `len`
 * whose entries are positive and sum to `len` (so a fresh even split is all 1s).
 * Missing/short/invalid inputs fall back to an even split.
 */
function normalize(input: number[] | undefined, len: number): number[] {
  if (len <= 0) return [];
  const base = Array.from({ length: len }, (_, i) =>
    input && typeof input[i] === "number" && input[i] > 0 ? input[i] : 1,
  );
  const sum = base.reduce((a, b) => a + b, 0);
  if (sum <= 0) return Array.from({ length: len }, () => 1);
  const scale = len / sum;
  return base.map((w) => w * scale);
}
