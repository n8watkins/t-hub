// A tile is a thin header (status dot, title, cwd, close) wrapping a TerminalView.
// It fills its grid cell and surfaces focus via a ring. Header click focuses the
// tile; the × detaches (closeTerminal); shift-clicking the × stops it (killTerminal).
//
// Drag-to-move/swap (PRD §5.3 manual mode): the header is a drag handle. Dragging
// it onto another tile reorders this tile into that tile's slot (a swap/insert
// within the active tab). The source terminal id rides along in the drag's
// dataTransfer, so no shared drag state is needed; the drop target reads it and
// calls the store's moveTile. The drag never touches the backend — only the
// visual order changes; the tmux session and any agent stay attached and alive.
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
  /** Render the terminal only when its tab is active; inactive tiles unmount
   *  the xterm/PTY client (tmux keeps running) per PRD §5.3 hidden tabs. */
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

  // True while another tile is being dragged over this one (drop highlight).
  const [dropTarget, setDropTarget] = useState(false);
  // True while *this* tile is the one being dragged (dim it).
  const [dragging, setDragging] = useState(false);

  const state: TerminalState = info?.state ?? "starting";
  const title = info?.title ?? terminalId;
  const cwd = info?.cwd ?? "";

  // --- Drag source (the header) ---
  const onDragStart = (e: DragEvent) => {
    e.dataTransfer.setData(TILE_DND_MIME, terminalId);
    e.dataTransfer.setData("text/plain", terminalId); // some platforms need text
    e.dataTransfer.effectAllowed = "move";
    setDragging(true);
  };
  const onDragEnd = () => setDragging(false);

  // --- Drop target (the whole tile) ---
  const isTileDrag = (e: DragEvent) =>
    e.dataTransfer.types.includes(TILE_DND_MIME);

  const onDragOver = (e: DragEvent) => {
    if (!isTileDrag(e)) return;
    e.preventDefault(); // allow the drop
    e.dataTransfer.dropEffect = "move";
    if (!dropTarget) setDropTarget(true);
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
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
      className={[
        "flex h-full min-h-0 w-full flex-col overflow-hidden rounded-sm bg-neutral-900",
        focused ? "ring-1 ring-emerald-500" : "border border-neutral-800",
        dropTarget ? "ring-2 ring-emerald-400" : "",
        dragging ? "opacity-40" : "",
      ].join(" ")}
    >
      {/* Header (~22px). Clicking anywhere here focuses the tile; it is also the
          drag handle for move/swap. */}
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

      {/* Body fills the rest of the cell; xterm fits to this box. Inactive tabs
          pass visible={false}, fully tearing down xterm while tmux lives on. */}
      <div className="min-h-0 flex-1 overflow-hidden">
        <TerminalView terminalId={terminalId} visible={visible} />
      </div>
    </div>
  );
}
