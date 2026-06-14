// A tile is a thin header (status dot, title, cwd, close) wrapping a TerminalView.
// It fills its grid cell and surfaces focus via a ring. Header click focuses the
// tile; the × detaches (closeTerminal); shift-clicking the × stops it (killTerminal).
//
// Drag-to-move (PRD §5.3 manual mode): the header is a drag handle. Dragging it
// onto ANY other tile (including a diagonal grid neighbor) pulls this tile out of
// the order and re-inserts it at the target's slot. The source terminal id rides
// along in the drag's dataTransfer, so the drop target reads it and calls the
// store's moveTile. The drag never touches the backend — only the visual order
// changes; the tmux session and any agent stay attached and alive.
//
// Reliability note: xterm's WebGL canvas / hidden textarea cover the tile body
// and, in the WebView, swallow HTML5 drag events so a naive drop never fires.
// While ANY tile drag is in progress (store.draggingTileId set), each tile lays
// a transparent overlay over its body that owns dragover/drop above xterm, so
// dropping onto a terminal works.
import { useState } from "react";
import type { DragEvent } from "react";
import type { TerminalId, TerminalState } from "../ipc/types";
import { useWorkspace } from "../store/workspace";
import { killTerminal } from "../ipc/client";
import { TerminalView } from "./Terminal";

/** dataTransfer MIME used to carry the dragged tile's terminal id. */
const TILE_DND_MIME = "application/x-termhub-tile";

export interface TileProps {
  terminalId: TerminalId;
  focused: boolean;
  /** Render the terminal only when visible. Shell v2 keeps every tile visible
   *  (even on inactive tabs) so xterm stays mounted and tab switches never
   *  reload a terminal; the canvas hides inactive tabs with CSS display. */
  visible: boolean;
  onFocus: () => void;
  onClose: () => void;
}

/** Status-dot color per lifecycle state (PRD §5.3 tile chrome). */
const DOT_CLASS: Record<TerminalState, string> = {
  starting: "bg-amber-500",
  live: "bg-emerald-500",
  detached: "bg-neutral-400",
  exited: "bg-neutral-600",
  error: "bg-red-500",
};

export function Tile({
  terminalId,
  focused,
  visible,
  onFocus,
  onClose,
}: TileProps) {
  // Subscribe to just this terminal's record so the header reflects live state.
  const info = useWorkspace((s) => s.terminals[terminalId]);
  const moveTile = useWorkspace((s) => s.moveTile);
  const setDraggingTile = useWorkspace((s) => s.setDraggingTile);
  // Is SOME tile being dragged right now? (Drives the drop overlay on every
  // tile so drops land above xterm.)
  const draggingTileId = useWorkspace((s) => s.draggingTileId);

  // True while another tile is being dragged over this one (drop highlight).
  const [dropTarget, setDropTarget] = useState(false);

  const state: TerminalState = info?.state ?? "starting";
  const title = info?.title ?? terminalId;
  const cwd = info?.cwd ?? "";

  const dragActive = draggingTileId !== null;
  const isSelfDragging = draggingTileId === terminalId;

  // --- Drag source (the header) ---
  const onDragStart = (e: DragEvent) => {
    e.dataTransfer.setData(TILE_DND_MIME, terminalId);
    e.dataTransfer.setData("text/plain", terminalId); // some platforms need text
    e.dataTransfer.effectAllowed = "move";
    setDraggingTile(terminalId);
  };
  const onDragEnd = () => {
    setDraggingTile(null);
    setDropTarget(false);
  };

  // --- Drop target (the overlay; falls back to the tile for non-xterm areas) ---
  const isTileDrag = (e: DragEvent) =>
    e.dataTransfer.types.includes(TILE_DND_MIME);

  const onDragOver = (e: DragEvent) => {
    if (!isTileDrag(e)) return;
    e.preventDefault(); // allow the drop
    e.dataTransfer.dropEffect = "move";
    if (!dropTarget && !isSelfDragging) setDropTarget(true);
  };
  const onDragLeave = (e: DragEvent) => {
    // Ignore leave events bubbling from children; only clear when truly leaving.
    if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
    setDropTarget(false);
  };
  const onDrop = (e: DragEvent) => {
    if (!isTileDrag(e)) return;
    e.preventDefault();
    setDropTarget(false);
    const sourceId = e.dataTransfer.getData(TILE_DND_MIME);
    if (sourceId && sourceId !== terminalId) moveTile(sourceId, terminalId);
  };

  return (
    <div
      className={[
        "relative flex h-full min-h-0 w-full flex-col overflow-hidden rounded-sm bg-neutral-900",
        focused ? "ring-1 ring-emerald-500" : "border border-neutral-800",
        dropTarget ? "ring-2 ring-emerald-400" : "",
        isSelfDragging ? "opacity-40" : "",
      ].join(" ")}
    >
      {/* Header (~22px). Clicking anywhere here focuses the tile; it is also the
          drag handle for move. */}
      <div
        draggable
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        onMouseDown={onFocus}
        className="flex h-[22px] shrink-0 cursor-grab select-none items-center gap-2 border-b border-neutral-800 bg-neutral-950/60 px-2 text-xs active:cursor-grabbing"
        title={cwd}
      >
        <span
          className={`h-2 w-2 shrink-0 rounded-full ${DOT_CLASS[state]}`}
          aria-label={state}
          title={state}
        />
        <span className="truncate text-neutral-200">{title}</span>
        {cwd && (
          <span className="min-w-0 flex-1 truncate text-neutral-500">{cwd}</span>
        )}
        {!cwd && <span className="flex-1" />}
        <button
          type="button"
          // Don't let the × start a drag; shift-click stops (kills) the session,
          // plain click detaches.
          draggable={false}
          onMouseDown={(e) => e.stopPropagation()}
          onDragStart={(e) => e.preventDefault()}
          onClick={(e) => {
            e.stopPropagation();
            if (e.shiftKey) void killTerminal(terminalId);
            else onClose();
          }}
          className="shrink-0 rounded px-1 leading-none text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200"
          title={"Detach (click) · Stop (shift-click)"}
          aria-label="Close terminal"
        >
          ×
        </button>
      </div>

      {/* Body fills the rest of the cell; xterm fits to this box. Shell v2 keeps
          visible=true on every tile so xterm stays mounted across tab switches. */}
      <div className="min-h-0 flex-1 overflow-hidden">
        <TerminalView terminalId={terminalId} visible={visible} />
      </div>

      {/* Drop overlay — only present while SOME tile is being dragged. It covers
          the whole tile (incl. the xterm canvas) so HTML5 dragover/drop fire
          reliably; without it the WebView's canvas swallows them. The source
          tile's own overlay is click/drop-through (pointer-events-none) so it
          never blocks the tiles beneath the cursor. */}
      {dragActive && (
        <div
          className={[
            "absolute inset-0 z-20",
            isSelfDragging ? "pointer-events-none" : "",
            dropTarget ? "bg-emerald-400/10" : "",
          ].join(" ")}
          onDragOver={onDragOver}
          onDragLeave={onDragLeave}
          onDrop={onDrop}
          aria-hidden
        />
      )}
    </div>
  );
}
