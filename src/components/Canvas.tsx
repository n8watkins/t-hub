// The canvas renders the active workspace tab as a responsive auto-grid of
// terminal tiles (PRD §5.2 tabs, §5.3 layout):
//   - On mount: listTerminals() seeds the store; onState() keeps tile chrome live.
//   - A thin tab strip lists every workspace tab; Ctrl/Cmd+Tab cycles, and tabs
//     can be created / renamed (double-click) / closed-when-empty.
//   - Each tab is a deterministic near-square grid sized from its tile count.
//   - Spawn (+ button, empty-state button, Ctrl/Cmd+T) inserts after the focused
//     tile in the active tab; Ctrl/Cmd+W detaches the focused tile.
//   - Manual mode: draggable gutters between rows/columns adjust their flex
//     ratios, persisted per tab (PRD §5.3 resize).
//   - Only the active tab's tiles render with visible=true; inactive tabs keep
//     their tiles mounted but visible=false, so xterm/PTY tears down while the
//     tmux session keeps running (fast reactivation, no resource churn).
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
  // (Shift reverses).
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
  }, [spawn, closeFocused, cycleTab, zoomIn, zoomOut, zoomReset, onToggleSidebar]);

  return (
    <div className="relative flex h-full w-full flex-col bg-neutral-950">
      <TabStrip />

      <div className="relative min-h-0 flex-1">
        {/* Every tab stays mounted so inactive tiles keep visible=false (xterm
            torn down, tmux alive). Only the active tab is displayed. */}
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          return (
            <div
              key={tab.id}
              className={active ? "absolute inset-0" : "hidden"}
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
      </div>

      {/* Persistent affordance to add more terminals to the active tab. */}
      <button
        type="button"
        onClick={() => void spawn()}
        title="New terminal (Ctrl/Cmd+T)"
        aria-label="New terminal"
        className="absolute bottom-3 right-3 z-30 flex h-9 w-9 cursor-pointer items-center justify-center rounded-full border border-neutral-700 bg-neutral-900/90 text-lg leading-none text-neutral-200 shadow-lg hover:border-emerald-600 hover:text-white"
      >
        +
      </button>
    </div>
  );
}

/** Centered call-to-action shown when the active tab has no tiles. */
function EmptyTab({ onSpawn }: { onSpawn: () => void }) {
  return (
    <div className="flex h-full w-full items-center justify-center bg-neutral-950">
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
// Workspace tab strip (PRD §5.2). Thin top strip listing every tab; click to
// activate, double-click to rename inline, × to close an empty tab, + to add.
// ---------------------------------------------------------------------------
function TabStrip() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const addTab = useWorkspace((s) => s.addTab);
  const renameTab = useWorkspace((s) => s.renameTab);
  const closeTab = useWorkspace((s) => s.closeTab);

  // id of the tab currently being renamed inline (null = none).
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const startRename = (id: string, name: string) => {
    setEditing(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (editing) renameTab(editing, draft);
    setEditing(null);
  };

  return (
    <div className="flex h-8 shrink-0 items-stretch gap-1 border-b border-neutral-800 bg-neutral-950 px-1 text-xs">
      <div className="flex min-w-0 flex-1 items-stretch gap-1 overflow-x-auto">
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          const closable = tabs.length > 1 && tab.order.length === 0;
          return (
            <div
              key={tab.id}
              onMouseDown={() => setActiveTab(tab.id)}
              onDoubleClick={() => startRename(tab.id, tab.name)}
              className={[
                "group flex shrink-0 cursor-pointer select-none items-center gap-1.5 rounded-t border-x border-t px-2.5",
                active
                  ? "border-neutral-700 bg-neutral-900 text-neutral-100"
                  : "border-transparent text-neutral-400 hover:bg-neutral-900/60 hover:text-neutral-200",
              ].join(" ")}
              title={tab.name}
            >
              <span
                className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                  active ? "bg-emerald-500" : "bg-neutral-600"
                }`}
              />
              {editing === tab.id ? (
                <input
                  autoFocus
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={commitRename}
                  onMouseDown={(e) => e.stopPropagation()}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitRename();
                    else if (e.key === "Escape") setEditing(null);
                  }}
                  className="w-24 bg-neutral-800 px-1 text-neutral-100 outline-none ring-1 ring-emerald-600"
                />
              ) : (
                <span className="max-w-[12rem] truncate">{tab.name}</span>
              )}
              {tab.order.length > 0 && (
                <span className="text-[10px] text-neutral-500">
                  {tab.order.length}
                </span>
              )}
              {closable && (
                <button
                  type="button"
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(tab.id);
                  }}
                  className="ml-0.5 hidden shrink-0 rounded px-0.5 leading-none text-neutral-500 hover:bg-neutral-700 hover:text-neutral-100 group-hover:inline"
                  title="Close empty tab"
                  aria-label={`Close ${tab.name}`}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
      </div>
      <button
        type="button"
        onClick={() => addTab()}
        className="my-1 shrink-0 rounded px-2 leading-none text-neutral-400 hover:bg-neutral-900 hover:text-neutral-100"
        title="New workspace tab"
        aria-label="New workspace tab"
      >
        +
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Tab grid + manual resize (PRD §5.3). Tiles are laid out in balanced rows; a
// draggable gutter between each adjacent pair of rows (and of columns within a
// row) adjusts their flex-grow weights, which are persisted on the tab.
// ---------------------------------------------------------------------------

/** Smallest flex weight a row/column may shrink to while dragging a gutter. */
const MIN_FLEX = 0.15;

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
  // A gutter sits between elements i and i+1; dragging it trades weight between
  // them proportionally to the pointer delta over the container's extent.
  const dragRef = useRef<{
    axis: "row" | "col";
    rowIdx: number; // for col drags, which row
    i: number; // left/top element index of the pair
    startPos: number;
    extentPx: number;
    aStart: number;
    bStart: number;
  } | null>(null);

  const onPointerMove = useCallback((e: PointerEvent) => {
    const d = dragRef.current;
    if (!d || d.extentPx <= 0) return;
    const pos = d.axis === "row" ? e.clientY : e.clientX;
    const total = d.aStart + d.bStart;
    // Convert px delta to weight delta (weights here sum to `total`).
    let delta = ((pos - d.startPos) / d.extentPx) * total;
    // Clamp so neither side drops below MIN_FLEX.
    const maxDelta = d.bStart - MIN_FLEX;
    const minDelta = -(d.aStart - MIN_FLEX);
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
      rowIdx: 0,
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
    <div ref={containerRef} className="flex h-full w-full flex-col gap-1 p-1">
      {layout.flatMap((row, r) => {
        const rowEl = (
          <div
            key={`row-${r}`}
            className="flex min-h-0 gap-1"
            style={{ flexGrow: rows[r] ?? 1, flexBasis: 0 }}
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
                    visible={active}
                    onFocus={() => onFocus(id)}
                    onClose={() => onClose(id)}
                  />
                </div>
              );
              if (c === 0) return [cell];
              // Column gutter (drag to resize the two adjacent tiles' widths).
              const gutter = (
                <div
                  key={`cg-${r}-${c}`}
                  role="separator"
                  aria-orientation="vertical"
                  onPointerDown={(e) => beginColDrag(r, c - 1, e)}
                  className="group relative z-10 -mx-0.5 w-1 shrink-0 cursor-col-resize"
                >
                  <div className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-transparent group-hover:bg-emerald-600/70" />
                </div>
              );
              return [gutter, cell];
            })}
          </div>
        );
        if (r === 0) return [rowEl];
        // Row gutter (drag to resize the two adjacent rows' heights).
        const gutter = (
          <div
            key={`rg-${r}`}
            role="separator"
            aria-orientation="horizontal"
            onPointerDown={(e) => beginRowDrag(r - 1, e)}
            className="group relative z-10 -my-0.5 h-1 shrink-0 cursor-row-resize"
          >
            <div className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-transparent group-hover:bg-emerald-600/70" />
          </div>
        );
        return [gutter, rowEl];
      })}
    </div>
  );
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
