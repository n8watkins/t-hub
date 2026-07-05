// Command catalog for the hybrid keymap (WS-3) — METADATA ONLY (no handlers).
//
// This is the single source of truth for which commands exist, their human
// labels, and grouping. It is deliberately split out from:
//   - store/keybindings.ts (which maps these ids -> key chords), and
//   - lib/keymapExecutor.ts (which maps these ids -> handlers that call store
//     methods).
// Keeping the id list here (with no store/IPC imports) lets both the store and
// the executor depend on it without a circular import.
//
// Every id below is migrated from a hotkey that USED to be hardcoded in
// Canvas.tsx (spawn, kill, cycle, jump 1-9, zoom in/out/reset, focus-region
// toggle), plus the NEW command palette. The tab-jump commands are enumerated
// 1..9 (rather than parameterized) so each is independently rebindable and shows
// up as its own row in the palette/settings.

/** The set of command identifiers. Exhaustive — adding one here forces the
 *  executor (lib/keymapExecutor.ts) to provide a handler (TS exhaustiveness on
 *  the Record). */
export const COMMAND_IDS = [
  "spawnTerminal",
  "closeTerminal",
  "cycleTileNext",
  "cycleTilePrev",
  "focusTab1",
  "focusTab2",
  "focusTab3",
  "focusTab4",
  "focusTab5",
  "focusTab6",
  "focusTab7",
  "focusTab8",
  "focusTab9",
  "zoomIn",
  "zoomOut",
  "zoomReset",
  "toggleFocusRegion",
  "commandPalette",
  "newPlainWorkspace",
  "newWorktreeWorkspace",
  "openWorktreesList",
  "toggleCaptainOverlay",
] as const;

export type CommandId = (typeof COMMAND_IDS)[number];

/** Display groups for the palette + settings list (purely presentational). */
export type CommandCategory =
  | "Terminals"
  | "Workspaces"
  | "Navigation"
  | "Zoom"
  | "App";

export interface CommandMeta {
  id: CommandId;
  /** Short human label shown in the palette + settings. */
  label: string;
  /** One-line description (palette secondary text / settings tooltip). */
  description: string;
  category: CommandCategory;
}

/** Per-command display metadata, in a sensible presentation order. */
export const COMMANDS: CommandMeta[] = [
  {
    id: "spawnTerminal",
    label: "New terminal",
    description: "Spawn a terminal after the focused tile",
    category: "Terminals",
  },
  {
    id: "closeTerminal",
    label: "Close terminal",
    description: "Kill the focused terminal's session",
    category: "Terminals",
  },
  {
    id: "newPlainWorkspace",
    label: "New workspace",
    description: "Open a new empty tab (no repo, no worktree)",
    category: "Workspaces",
  },
  {
    id: "newWorktreeWorkspace",
    label: "New worktree workspace",
    description:
      "Branch the focused repo into a sibling worktree and open it in a new tab",
    category: "Workspaces",
  },
  {
    id: "openWorktreesList",
    label: "List worktrees",
    description: "Show the focused repo's worktrees to re-open or remove",
    category: "Workspaces",
  },
  {
    id: "cycleTileNext",
    label: "Next terminal",
    description: "Focus the next tile (across all workspaces)",
    category: "Navigation",
  },
  {
    id: "cycleTilePrev",
    label: "Previous terminal",
    description: "Focus the previous tile (across all workspaces)",
    category: "Navigation",
  },
  {
    id: "toggleFocusRegion",
    label: "Toggle focus: terminal / sidebar",
    description: "Move keyboard focus between the terminal area and the sidebar",
    category: "Navigation",
  },
  ...([1, 2, 3, 4, 5, 6, 7, 8, 9] as const).map(
    (n): CommandMeta => ({
      id: `focusTab${n}` as CommandId,
      label: `Jump to workspace ${n}`,
      description: `Activate the workspace tab at position ${n}`,
      category: "Navigation",
    }),
  ),
  {
    id: "zoomIn",
    label: "Zoom in",
    description: "Increase terminal font size",
    category: "Zoom",
  },
  {
    id: "zoomOut",
    label: "Zoom out",
    description: "Decrease terminal font size",
    category: "Zoom",
  },
  {
    id: "zoomReset",
    label: "Reset zoom",
    description: "Reset terminal font size",
    category: "Zoom",
  },
  {
    id: "commandPalette",
    label: "Command palette",
    description: "Open the fuzzy command palette",
    category: "App",
  },
  {
    id: "toggleCaptainOverlay",
    label: "Toggle captain overlay",
    description:
      "Summon the pinned captain terminal in a floating panel over any workspace",
    category: "App",
  },
];

/** Quick id -> metadata lookup. */
export const COMMAND_BY_ID: Record<CommandId, CommandMeta> = Object.fromEntries(
  COMMANDS.map((c) => [c.id, c]),
) as Record<CommandId, CommandMeta>;
