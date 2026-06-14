// A tile is a thin header (status dot, title, cwd, close) above an EMPTY body
// placeholder. It fills its grid cell and surfaces selection as a subtle theme-
// accent glow. The terminal's xterm body is NOT a child of the tile: #20 renders
// each terminal once in a persistent pool overlay (TerminalPool.tsx) and
// positions it over this tile's placeholder, so moving/resizing the tile only
// repositions the pooled terminal — it is never remounted/reattached (no flash).
// Pressing the header focuses the tile; the × detaches (closeTerminal), and
// shift-clicking the × stops it (killTerminal).
//
// Drag-to-move (PRD §5.3 manual mode): the header is a drag HANDLE built on
// POINTER events (not HTML5 drag-and-drop, which dies over xterm's WebGL canvas
// in WebView2 — see src/lib/pointerDrag.ts). Dragging the header and releasing:
//   - over ANY other tile  -> moveTile (swap/re-insert at the target's slot, in
//     any direction including a diagonal grid neighbor);
//   - over a workspace TAB -> moveTileToTab (move this terminal to that tab).
// The drag is resolved with `document.elementFromPoint` + `closest(...)`, so no
// drop overlay is needed: while a drag is active the body carries
// data-th-dragging (index.css) which makes terminals ignore the pointer, so the
// point resolves to the owning tile (data-tile-id) rather than the canvas. The
// drag never touches the backend — only the visual order changes; the tmux
// session and any agent stay attached and alive.
import type { PointerEvent as ReactPointerEvent } from "react";
import type { TerminalId, TerminalState } from "../ipc/types";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { useTheme } from "../store/theme";
import { killTerminal } from "../ipc/client";
import { useTerminalSlot } from "./TerminalPool";
import { startPointerDrag } from "../lib/pointerDrag";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";

export interface TileProps {
  terminalId: TerminalId;
  focused: boolean;
  /** Retained for API compatibility; the xterm body no longer lives in the tile.
   *  #20 pools every terminal in a persistent, never-reparented overlay (see
   *  TerminalPool.tsx); the tile renders only its header chrome plus a ref'd
   *  placeholder the pool positions the pooled terminal over. The pool decides
   *  visibility (active tab + real placeholder rect), so this prop is unused. */
  visible?: boolean;
  onFocus: () => void;
  onClose: () => void;
}

/**
 * Status-dot color per lifecycle state (PRD §5.3 tile chrome). These are themed:
 * each maps to a CSS var the theme store writes, so retheming the dot palette is
 * live. Used as an inline `backgroundColor` (Tailwind can't take a dynamic var
 * key per state).
 */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

/**
 * Resolve which TermHub drop target sits under a viewport point mid-drag. A
 * workspace tab (data-tab-id) wins over a tile (data-tile-id); elementFromPoint
 * returns the topmost element (often xterm's canvas), so we walk up to the
 * owning tile/tab with `closest`.
 */
function dropTargetAt(
  x: number,
  y: number,
): { tileId: string | null; tabId: string | null } {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  if (!el) return { tileId: null, tabId: null };
  const tabEl = el.closest<HTMLElement>("[data-tab-id]");
  if (tabEl) return { tileId: null, tabId: tabEl.getAttribute("data-tab-id") };
  const tileEl = el.closest<HTMLElement>("[data-tile-id]");
  if (tileEl) return { tileId: tileEl.getAttribute("data-tile-id"), tabId: null };
  return { tileId: null, tabId: null };
}

export function Tile({ terminalId, focused, onFocus, onClose }: TileProps) {
  // Register this tile's body box with the terminal pool: the pooled (persistent)
  // <TerminalView> for this id is positioned over this placeholder. Moving the
  // tile to another tab/slot just repositions the pooled terminal — it is never
  // remounted, so there's no reload/flash (#20).
  const slotRef = useTerminalSlot(terminalId);
  // Subscribe to just this terminal's record so the header reflects live state.
  const info = useWorkspace((s) => s.terminals[terminalId]);
  // The user-set label (if any) for this terminal; the highest-priority input to
  // the friendly display name. Subscribed so an inline rename re-renders the head.
  const userLabel = useWorkspace((s) => s.labels[terminalId]);
  const moveTile = useWorkspace((s) => s.moveTile);
  const moveTileToTab = useWorkspace((s) => s.moveTileToTab);
  const setDraggingTile = useWorkspace((s) => s.setDraggingTile);
  const setDropTile = useWorkspace((s) => s.setDropTile);
  const setDropTab = useWorkspace((s) => s.setDropTab);
  // Which tile is the drag source / current drop target (for visual state).
  const draggingTileId = useWorkspace((s) => s.draggingTileId);
  const dropTileId = useWorkspace((s) => s.dropTileId);
  // Themed header behavior: hide the cwd, and/or collapse the header to a
  // hover-reveal hairline. Subscribed so a live toggle in the editor re-renders.
  const showCwd = useTheme((s) => s.active.chrome.showCwd);
  const headerOnHover = useTheme((s) => s.active.chrome.headerOnHover);
  const showTileHeader = useTheme((s) => s.active.chrome.showTileHeader);

  const state: TerminalState = info?.state ?? "starting";
  const cwd = info?.cwd ?? "";
  // Friendly display name (user label > derived preset·cwd > short id). The short
  // id (terminalId) is always shown as a dimmed secondary detail beside it.
  const label = deriveLabel({
    id: terminalId,
    label: userLabel,
    title: info?.title,
    cwd: info?.cwd,
  });
  // True when the friendly label IS just the short id (no preset/cwd/label to go
  // on) — then we don't repeat the id as a secondary detail.
  const showShortId = label !== terminalId;

  const isSelfDragging = draggingTileId === terminalId;
  const isDropTarget = dropTileId === terminalId && draggingTileId !== terminalId;

  // --- Drag source (the header), pointer-based ---
  const onHeaderPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0) return; // primary (left) button only
    onFocus(); // pressing the header selects the tile right away
    const sourceId = terminalId;
    let ghost: DragGhost | null = null;
    startPointerDrag(e.clientX, e.clientY, {
      onBegin: () => {
        setDraggingTile(sourceId);
        // Make terminals pointer-inert for the drag (index.css) so the gesture
        // tracks over them and elementFromPoint resolves to tiles, not canvases.
        document.body.dataset.thDragging = "1";
        // A floating frame of the tile that follows the cursor (clear "I'm
        // carrying this tile" feedback the dimmed source alone doesn't give).
        ghost = createDragGhost({
          title: label,
          subtitle: cwd || undefined,
          width: 240,
          bodyHeight: 120,
        });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        const { tileId, tabId } = dropTargetAt(x, y);
        setDropTab(tabId);
        setDropTile(tileId && tileId !== sourceId ? tileId : null);
      },
      onEnd: (x, y, committed) => {
        const target = committed
          ? dropTargetAt(x, y)
          : { tileId: null, tabId: null };
        ghost?.destroy();
        ghost = null;
        delete document.body.dataset.thDragging;
        setDraggingTile(null);
        setDropTile(null);
        setDropTab(null);
        if (!committed) return;
        if (target.tabId) {
          moveTileToTab(sourceId, target.tabId);
        } else if (target.tileId && target.tileId !== sourceId) {
          moveTile(sourceId, target.tileId);
        }
      },
    });
  };

  return (
    <div
      // data-tile-id makes this the drop target a drag resolves to via
      // elementFromPoint + closest (the canvas inside is pointer-inert mid-drag).
      data-tile-id={terminalId}
      // Header visibility + hover-reveal are driven purely by CSS off these data
      // attributes (see index.css) so toggling them is instant with no extra
      // React state and the stylesheet can own the header's height.
      data-tile-header={showTileHeader ? "1" : "0"}
      data-header-hover={headerOnHover ? "1" : "0"}
      className={[
        "relative flex h-full min-h-0 w-full flex-col overflow-hidden",
        isSelfDragging ? "opacity-40" : "",
      ].join(" ")}
      style={{
        backgroundColor: "var(--th-tile-bg)",
        borderRadius: "var(--th-radius)",
        // Reserve a 1px border always so selecting never reflows the tile by 1px.
        // Selection (#5) is a SUBTLE accent "lit up", not a hard ring: an
        // accent-tinted border plus a soft outer glow. A live drop target instead
        // gets a crisp accent inset ring so the drop reads clearly.
        border: "1px solid",
        borderColor: isDropTarget
          ? "var(--th-accent)"
          : focused
            ? "color-mix(in srgb, var(--th-focus-ring) 55%, var(--th-border))"
            : "var(--th-border)",
        boxShadow: isDropTarget
          ? "inset 0 0 0 2px var(--th-accent)"
          : focused
            ? "0 0 0 1px color-mix(in srgb, var(--th-focus-ring) 40%, transparent), 0 0 16px -4px color-mix(in srgb, var(--th-focus-ring) 60%, transparent)"
            : "none",
      }}
    >
      {/* Header. Pressing anywhere here focuses the tile; it is also the pointer
          drag handle for move. Height + colors + visibility are themed; the
          `th-tile-header` class lets index.css drive the hover-reveal mode. */}
      <div
        onPointerDown={onHeaderPointerDown}
        // Height / display / hover-reveal are driven from index.css (off the
        // `th-tile-header` class + the parent's data-header-hover) so the
        // stylesheet's hover rule can override the height — an inline height
        // would win by specificity and break the reveal. Only the per-instance
        // colors/font live inline here.
        className="th-tile-header flex shrink-0 cursor-grab touch-none select-none items-center gap-2 border-b px-2 active:cursor-grabbing"
        style={{
          backgroundColor: "var(--th-header-bg)",
          borderColor: "var(--th-border)",
          fontSize: "var(--th-font-size)",
        }}
        title={cwd}
      >
        <span
          // Lifecycle dot, intentionally small/low-key (#5) so it no longer
          // reads as a "selected" marker; hover for the exact state.
          className="h-1.5 w-1.5 shrink-0 rounded-full"
          style={{ backgroundColor: DOT_VAR[state] }}
          aria-label={state}
          title={`Terminal state: ${state}`}
        />
        {/* Friendly label, prominent; the raw 8-char id follows it faint as a
            secondary detail so the session id stays discoverable without being
            the headline (only shown when the label isn't already just that id). */}
        <span className="truncate" style={{ color: "var(--th-fg)" }}>
          {label}
        </span>
        {showShortId && (
          <span
            className="shrink-0 font-mono text-[0.85em]"
            style={{ color: "var(--th-fg-muted)" }}
            title={`Session ${terminalId}`}
          >
            {terminalId}
          </span>
        )}
        {showCwd && cwd && (
          <span
            className="min-w-0 flex-1 truncate"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {cwd}
          </span>
        )}
        {(!showCwd || !cwd) && <span className="flex-1" />}
        <button
          type="button"
          // Don't let the × start a drag/focus; shift-click stops (kills) the
          // session, plain click detaches.
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            if (e.shiftKey) void killTerminal(terminalId);
            else onClose();
          }}
          className="shrink-0 rounded px-1 leading-none text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200"
          style={{ color: "var(--th-fg-muted)" }}
          title={"Detach (click) · Stop (shift-click)"}
          aria-label="Close terminal"
        >
          ×
        </button>
      </div>

      {/* Body placeholder: an empty box marking where the terminal should sit.
          The actual xterm is rendered ONCE in the persistent pool overlay
          (TerminalPool.tsx) and positioned over this box, so moving/resizing the
          tile never remounts or reattaches it (#20). Kept data-tile-id-covered by
          the parent so drag drop-resolution (elementFromPoint) still lands here. */}
      <div ref={slotRef} className="min-h-0 flex-1 overflow-hidden" />
    </div>
  );
}
