// The command registry (WS-3) — maps each CommandId to a handler that performs
// the action by calling EXISTING store methods. This is the one place a command
// turns into a side effect; the Canvas keydown handler, the prefix handler, and
// the command palette all dispatch THROUGH here, so behavior is identical no
// matter how a command was triggered.
//
// IMPORTANT: handlers only ever read/call already-published store methods
// (useWorkspace.getState()…, usePanels.getState()…) and the existing IPC client.
// We do NOT add methods to or otherwise edit the stores — this module owns the
// command<->action wiring exclusively.
//
// Every handler below is a faithful migration of an action that used to live
// inline in Canvas.tsx's onKey:
//   spawnTerminal    -> spawnTerminal IPC + addAfterFocused   (Canvas `spawn()`)
//   closeTerminal    -> deleteTerminal(focusedId)             (Canvas `closeFocused()`)
//   cycleTileNext/Prev -> cycleTileGlobal(+1/-1) OR cycleTab when sidebar-focused
//   focusTab1..9     -> setActiveTabByIndex(n-1)
//   zoomIn/Out/Reset -> zoomIn/zoomOut/zoomReset
//   toggleFocusRegion-> toggleFocusRegion (+ reveal/focus the sidebar)  RELOCATED
//                       off ctrl+b (now the prefix) to a direct chord.
//   commandPalette   -> open the fuzzy palette (NEW)
import { useWorkspace } from "../store/workspace";
import { useCaptain } from "../store/captain";
import { spawnTerminal } from "../ipc/client";
import { tlog } from "./diag";
import type { CommandId } from "./commands";

// ---------------------------------------------------------------------------
// Cross-component hooks the executor needs but cannot import directly without a
// cycle (the palette / Canvas import the executor, not the other way round). A
// component registers its callback once on mount; a no-op stands in until then.
// ---------------------------------------------------------------------------

let openPalette: (() => void) | null = null;
/** CommandPalette registers its opener here on mount (App root). */
export function registerPaletteOpener(fn: (() => void) | null): void {
  openPalette = fn;
}

let openWorktreePrompt: ((cwd: string | undefined) => void) | null = null;
/** WorktreePrompt registers its opener here on mount (App root). The executor
 *  passes the focused tile's LIVE cwd so the prompt can anchor repo resolution.
 *  Mirrors registerPaletteOpener (no import from the component → no cycle). */
export function registerWorktreePromptOpener(
  fn: ((cwd: string | undefined) => void) | null,
): void {
  openWorktreePrompt = fn;
}

let openWorktreesList: ((cwd: string | undefined) => void) | null = null;
/** WorktreesList registers its opener here on mount (App root). The executor
 *  passes the focused tile's LIVE cwd so the modal can resolve that repo's
 *  worktrees. Mirrors registerWorktreePromptOpener (no import from the component
 *  → no cycle). */
export function registerWorktreesListOpener(
  fn: ((cwd: string | undefined) => void) | null,
): void {
  openWorktreesList = fn;
}

let revealSidebarAndFocus: (() => void) | null = null;
/**
 * Canvas registers the side effect that, when focus moves to the sidebar region,
 * reveals a hidden sidebar and focuses its nav target — the same behavior the old
 * inline Ctrl+B handler performed via `onFocusSidebar` + `focusSidebarTarget`.
 * Kept here (not in the handler) because the reveal callback (`onFocusSidebar`)
 * is a Canvas prop and `focusSidebarTarget` is a Canvas-local helper.
 */
export function registerSidebarFocus(fn: (() => void) | null): void {
  revealSidebarAndFocus = fn;
}

// ---------------------------------------------------------------------------
// Handlers. Each calls existing store methods only.
// ---------------------------------------------------------------------------

/** Spawn a plain shell after the focused tile and focus it — exactly Canvas's
 *  `spawn()` (no startup command = today's bare login shell, no regression). */
async function doSpawn(): Promise<void> {
  try {
    const info = await spawnTerminal({});
    tlog(
      "spawn",
      `spawned ${info.id} cmd=(shell) ` +
        `tiles-before=${useWorkspace
          .getState()
          .tabs.reduce((n, t) => n + t.order.length, 0)}`,
    );
    useWorkspace.getState().addAfterFocused(info);
  } catch (err) {
    console.error("spawnTerminal failed", err);
  }
}

/** Kill the focused session (kill + drop tile) — Canvas's `closeFocused()`. No
 *  busy gate (an explicit keybind is a deliberate action). */
function doCloseFocused(): void {
  const { focusedId, deleteTerminal } = useWorkspace.getState();
  if (!focusedId) return;
  deleteTerminal(focusedId);
}

/** Cycle the focused tile/workspace in `dir`. Mirrors the old Ctrl+Tab branch:
 *  when the SIDEBAR region is focused, cycle WORKSPACES; otherwise cycle TILES
 *  across every workspace. */
function doCycle(dir: 1 | -1): void {
  const s = useWorkspace.getState();
  if (s.focusedRegion === "sidebar") s.cycleTab(dir);
  else s.cycleTileGlobal(dir);
}

/** Jump to the workspace tab at 0-based `index` — Canvas's Ctrl+1..9 branch. */
function doFocusTab(index: number): void {
  useWorkspace.getState().setActiveTabByIndex(index);
}

/** Toggle nav focus between the terminal area and the sidebar (RELOCATED off
 *  ctrl+b). Reveals/focuses the sidebar via the Canvas-registered side effect
 *  when moving there — same as the old inline handler. */
function doToggleFocusRegion(): void {
  const region = useWorkspace.getState().toggleFocusRegion();
  if (region === "sidebar") revealSidebarAndFocus?.();
}

/** New PLAIN workspace (WS-9c) — a fresh empty tab, no repo, no worktree. Just
 *  `addTab()` (which creates + activates the tab); the design's `Ctrl+B c`. */
function doNewPlainWorkspace(): void {
  useWorkspace.getState().addTab();
}

/** New WORKTREE workspace (WS-9c) — the design's `Ctrl+B w`. Capture the focused
 *  tile's LIVE cwd (the repo anchor) and open the branch prompt; the prompt drives
 *  the rest (resolve target -> addWorktreeWorkspace, with no-repo/error inline).
 *  A missing cwd is fine — the prompt opens and resolution falls through to
 *  no-repo. */
function doNewWorktreeWorkspace(): void {
  const id = useWorkspace.getState().focusedId;
  const cwd = id ? useWorkspace.getState().terminals[id]?.cwd : undefined;
  openWorktreePrompt?.(cwd);
}

/** Open the WORKTREES LIST (WS-9e) — re-open or remove an existing worktree.
 *  Capture the focused tile's LIVE cwd (the repo to list) and open the modal; it
 *  resolves the repo via gitWorktreeList and drives the rest. A missing cwd is
 *  fine — the modal resolves to a "not in a repo" empty state. */
function doOpenWorktreesList(): void {
  const id = useWorkspace.getState().focusedId;
  const cwd = id ? useWorkspace.getState().terminals[id]?.cwd : undefined;
  openWorktreesList?.(cwd);
}

/** The registry: every CommandId -> its handler. The Record type makes this
 *  exhaustive — a new CommandId won't compile until it has a handler here. */
const HANDLERS: Record<CommandId, () => void> = {
  spawnTerminal: () => void doSpawn(),
  closeTerminal: doCloseFocused,
  cycleTileNext: () => doCycle(1),
  cycleTilePrev: () => doCycle(-1),
  focusTab1: () => doFocusTab(0),
  focusTab2: () => doFocusTab(1),
  focusTab3: () => doFocusTab(2),
  focusTab4: () => doFocusTab(3),
  focusTab5: () => doFocusTab(4),
  focusTab6: () => doFocusTab(5),
  focusTab7: () => doFocusTab(6),
  focusTab8: () => doFocusTab(7),
  focusTab9: () => doFocusTab(8),
  zoomIn: () => useWorkspace.getState().zoomIn(),
  zoomOut: () => useWorkspace.getState().zoomOut(),
  zoomReset: () => useWorkspace.getState().zoomReset(),
  toggleFocusRegion: doToggleFocusRegion,
  commandPalette: () => openPalette?.(),
  newPlainWorkspace: doNewPlainWorkspace,
  newWorktreeWorkspace: doNewWorktreeWorkspace,
  openWorktreesList: doOpenWorktreesList,
  // Captain overlay (captain-list): closed -> summon the active captain;
  // already summoned -> CYCLE to the next pinned captain in MRU order (Esc
  // dismisses). The store action owns the focus save/restore contract.
  toggleCaptainOverlay: () => useCaptain.getState().toggleOverlay(),
  // Keyboard parity for the tile-header context item: pin/unpin the FOCUSED
  // tile without reaching for the mouse (palette-only by default). Pinning is
  // ADDITIVE - other captains stay pinned.
  pinCaptainFocused: () => {
    const id = useWorkspace.getState().focusedId;
    if (id) useCaptain.getState().toggleCaptain(id);
  },
};

/** Run a command by id. Safe to call from any trigger (Canvas keydown, the
 *  prefix handler, the palette). Unknown ids are ignored. */
export function runCommand(id: CommandId): void {
  const fn = HANDLERS[id];
  if (fn) fn();
}
