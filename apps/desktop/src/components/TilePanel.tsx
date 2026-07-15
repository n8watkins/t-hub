// TilePanel — the per-tile body SWITCHER for the non-terminal views.
//
// A project tile (Tile.tsx) is a little workbench with a Terminal / Files /
// Run + Preview / Board tab bar. The Terminal view is special: its xterm is not
// a child
// of the tile — the persistent pool (TerminalPool.tsx) renders each terminal
// once in an overlay and positions it over the tile's empty placeholder, so a
// move/resize never reloads it. The other views are ordinary React
// surfaces that DO live inside the tile body; this component renders whichever
// one the tile's `usePanels` tab selects.
//
// Mounting contract:
//   - files   -> <FilePanel root={cwd} />            (this project's tree/reader)
//   - preview -> <RunPreviewPanel terminalId cwd />
//                 One guided managed-runner, output, and preview lifecycle.
//   - board   -> <BoardPanel terminalId cwd />
//                 Resolves the durable Project and its protected Powder binding
//                 through one backend snapshot. Credentials never enter the UI.
//
// Tile.tsx only renders TilePanel when the active tab is NOT "terminal" (for the
// terminal tab it renders the pool placeholder instead), so this component never
// needs a "terminal" branch.
import type { ReactElement } from "react";
import type { TerminalId } from "../ipc/types";
import type { PanelTab } from "../store/panels";
import { FilePanel } from "./FilePanel";
import { BoardPanel } from "./BoardPanel";
import { RunPreviewPanel } from "./RunPreviewPanel";

export interface TilePanelProps {
  /** The terminal/project this tile belongs to (keys all per-tile panel state). */
  terminalId: TerminalId;
  /** This project's working directory.
   *  It roots Files and scopes the managed runner. */
  cwd: string;
  /** Which non-terminal view to render (the tile's active panel tab). */
  tab: Exclude<PanelTab, "terminal">;
  /** Narrow (split) container -> use FilePanel's compact stacked layout. False
   *  when the panel is expanded to fill the tile (roomy side-by-side). */
  compact?: boolean;
}

/**
 * Render the chosen non-terminal surface for a tile, scoped to its own cwd.
 * Each branch is a self-contained surface scoped by the focused terminal.
 */
export function TilePanel({
  terminalId,
  cwd,
  tab,
  compact = false,
}: TilePanelProps): ReactElement {
  switch (tab) {
    case "files":
      return (
        <FilePanel root={cwd || undefined} terminalId={terminalId} compact={compact} />
      );
    case "preview":
      return <RunPreviewPanel terminalId={terminalId} cwd={cwd} />;
    case "board":
      return <BoardPanel terminalId={terminalId} cwd={cwd} />;
  }
}
