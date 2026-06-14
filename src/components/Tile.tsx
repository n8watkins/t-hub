// A tile is a thin header (status dot, title, cwd, close) wrapping a TerminalView.
// It fills its grid cell and surfaces focus via a ring. Header click focuses the
// tile; the × detaches (closeTerminal); shift-clicking the × stops it (killTerminal).
import type { TerminalId, TerminalState } from "../ipc/types";
import { useWorkspace } from "../store/workspace";
import { killTerminal } from "../ipc/client";
import { TerminalView } from "./Terminal";

export interface TileProps {
  terminalId: TerminalId;
  focused: boolean;
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

export function Tile({ terminalId, focused, onFocus, onClose }: TileProps) {
  // Subscribe to just this terminal's record so the header reflects live state.
  const info = useWorkspace((s) => s.terminals[terminalId]);

  const state: TerminalState = info?.state ?? "starting";
  const title = info?.title ?? terminalId;
  const cwd = info?.cwd ?? "";

  return (
    <div
      className={[
        "flex h-full min-h-0 w-full flex-col overflow-hidden rounded-sm bg-neutral-900",
        focused
          ? "ring-1 ring-emerald-500"
          : "border border-neutral-800",
      ].join(" ")}
    >
      {/* Header (~22px). Clicking anywhere here focuses the tile. */}
      <div
        onMouseDown={onFocus}
        className="flex h-[22px] shrink-0 select-none items-center gap-2 border-b border-neutral-800 bg-neutral-950/60 px-2 text-xs"
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
          // Shift-click stops (kills) the session; plain click detaches.
          onMouseDown={(e) => e.stopPropagation()}
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

      {/* Body fills the rest of the cell; xterm fits to this box. */}
      <div className="min-h-0 flex-1 overflow-hidden">
        <TerminalView terminalId={terminalId} visible />
      </div>
    </div>
  );
}
