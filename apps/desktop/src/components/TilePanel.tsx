// TilePanel — the per-tile body SWITCHER for the non-terminal views.
//
// A project tile (Tile.tsx) is a little workbench with a Terminal / Files /
// Preview / Dev tab bar. The Terminal view is special: its xterm is NOT a child
// of the tile — the persistent pool (TerminalPool.tsx) renders each terminal
// once in an overlay and positions it over the tile's empty placeholder, so a
// move/resize never reloads it. The OTHER three views are ordinary React
// surfaces that DO live inside the tile body; this component renders whichever
// one the tile's `usePanels` tab selects.
//
// Mounting contract:
//   - files   -> <FilePanel root={cwd} />            (this project's tree/reader)
//   - preview -> <WebPreview initialUrl={devUrl ?? previewUrl} />
//                 The Preview tab prefers the LIVE dev-server URL the Dev runner
//                 publishes (usePanels.devUrl), falling back to a URL the user
//                 last committed in the bar (usePanels.previewUrl). WebPreview
//                 follows a changing `initialUrl` (see its initialUrl effect), so
//                 a freshly-detected dev server loads automatically.
//   - dev     -> <DevTab terminalId cwd/>            (Agent C owns its body; we
//                 only mount it — do NOT edit DevTab.tsx)
//
// Tile.tsx only renders TilePanel when the active tab is NOT "terminal" (for the
// terminal tab it renders the pool placeholder instead), so this component never
// needs a "terminal" branch.
import type { ReactElement } from "react";
import type { TerminalId } from "../ipc/types";
import { usePanels, type PanelTab } from "../store/panels";
import { FilePanel } from "./FilePanel";
import { WebPreview } from "./WebPreview";
import { DevTab } from "./DevTab";

export interface TilePanelProps {
  /** The terminal/project this tile belongs to (keys all per-tile panel state). */
  terminalId: TerminalId;
  /** This project's working directory — roots Files and scopes Dev. */
  cwd: string;
  /** Which non-terminal view to render (the tile's active panel tab). */
  tab: Exclude<PanelTab, "terminal">;
  /** Narrow (split) container -> use FilePanel's compact stacked layout. False
   *  when the panel is expanded to fill the tile (roomy side-by-side). */
  compact?: boolean;
}

/**
 * Render the chosen non-terminal surface for a tile, scoped to its own cwd.
 * Each branch is a self-contained surface; only the Preview tab reaches into the
 * panels store (for the live/last URL) — the others are driven purely by props.
 */
export function TilePanel({
  terminalId,
  cwd,
  tab,
  compact = false,
}: TilePanelProps): ReactElement {
  // Live dev-server URL (published by the Dev runner) preferred over the last
  // URL the user typed in this tile's Preview bar. Subscribed narrowly so only
  // a change to THIS tile's URLs re-renders the Preview surface. Read at the top
  // level (hooks can't sit inside the switch) but only consumed by Preview.
  const devUrl = usePanels((s) => s.devUrl[terminalId]);
  const previewUrl = usePanels((s) => s.previewUrl[terminalId]);

  switch (tab) {
    case "files":
      return <FilePanel root={cwd || undefined} compact={compact} />;
    case "preview":
      return <WebPreview initialUrl={devUrl ?? previewUrl ?? undefined} />;
    case "dev":
      // Mount-only: Agent C implements DevTab's body. Keep props stable.
      return <DevTab terminalId={terminalId} cwd={cwd} />;
  }
}
