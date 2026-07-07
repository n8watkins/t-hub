// A tile is a thin header (status dot, title, cwd, close) above an EMPTY body
// placeholder. It fills its grid cell and surfaces selection as a subtle theme-
// accent glow. The terminal's xterm body is NOT a child of the tile: #20 renders
// each terminal once in a persistent pool overlay (TerminalPool.tsx) and
// positions it over this tile's placeholder, so moving/resizing the tile only
// repositions the pooled terminal — it is never remounted/reattached (no flash).
// Pressing the header focuses the tile. The header's × KILLS this terminal's
// tmux session (feat/workspaces-lifecycle): durable Claude session history makes
// the old "detach + keep tmux alive" duality unnecessary, so the single close
// control now ends the session for good (deleteTerminal -> killTerminal). It is
// confirmed FIRST only when the session looks BUSY (a running dev server, see the
// busy note below); an idle session is killed immediately with no dialog. The
// conversation is still recoverable afterward from the Recent list.
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
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { TerminalId, TerminalState } from "../ipc/types";
import { useWorkspace, deriveLabel, tabIdForTerminal } from "../store/workspace";
import { useTheme } from "../store/theme";
import {
  usePanels,
  type PanelTab,
  DEFAULT_SPLIT_RATIO,
  clampSplitRatio,
} from "../store/panels";
import { useTerminalSlot } from "./TerminalPool";
import { TilePanel } from "./TilePanel";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";
import { clientForTerminal } from "../store/clientType";
import { useAutoContinue } from "../store/autoContinue";
import { ContextMeter } from "./ContextMeter";
import { useContextPctForTile, sessionNameForTerminal } from "../store/sessionContext";
import {
  useSupervision,
  tmuxSessionMidTurn,
  sessionStatusForTmux,
} from "../store/supervision";
import { useActivity } from "../store/activity";
import { StatusIndicator, terminalVariant } from "./StatusIndicator";
import { startPointerDrag, type PointerDragCanceller } from "../lib/pointerDrag";
import { resolveDropTarget } from "../lib/dropTarget";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { refreshTerminal } from "../lib/repaint";
import { useCaptain } from "../store/captain";
import { ConfirmDialog } from "./ConfirmDialog";
import { gitInfo, type GitInfo } from "../ipc/git";
import { runWhenIdle } from "../lib/windowInteraction";
import { Anchor, GitBranch } from "lucide-react";

/** Poll git facts (branch / worktree / dirty count) for a tile's cwd. Refreshes
 *  on mount and whenever the window regains focus (cheap; the backend best-efforts
 *  a non-repo to isRepo:false). Returns null until the first result. */
function useGitInfo(cwd: string): GitInfo | null {
  const [info, setInfo] = useState<GitInfo | null>(null);
  useEffect(() => {
    if (!cwd) {
      setInfo(null);
      return;
    }
    let alive = true;
    const load = () => {
      gitInfo(cwd)
        .then((g) => alive && setInfo(g))
        .catch(() => alive && setInfo(null));
    };
    load();
    // Defer the focus refresh past an active window drag (cold-first-drag fix).
    const onFocus = () => runWhenIdle(load);
    window.addEventListener("focus", onFocus);
    // Poll so a branch switch in the SAME directory (`git switch` without a cwd
    // change) is eventually reflected — the cwd-keyed effect alone wouldn't re-fire.
    // Skip while hidden, and poll INFREQUENTLY: each tick spawns a `git` (a wsl.exe
    // on Windows) per tile, and GIT_INFO_TTL (3.5s) is SHORTER than this interval, so
    // every tick is a cache MISS → a real spawn. At 5s × a wall of tiles that was a
    // periodic freeze (8 wsl.exe spawns every 5s); 30s + focus-refresh keeps the chip
    // fresh enough without the storm. (An in-place branch switch shows on focus or
    // within 30s.)
    const poll = window.setInterval(() => {
      if (!document.hidden) load();
    }, 30000);
    return () => {
      alive = false;
      window.removeEventListener("focus", onFocus);
      window.clearInterval(poll);
    };
  }, [cwd]);
  return info;
}

/** The tile-header tab bar order + labels. Terminal is the default view. The
 *  Dev view runs the project's dev server and feeds its URL to Preview. */
const PANEL_TABS: { id: PanelTab; label: string }[] = [
  { id: "terminal", label: "Terminal" },
  { id: "files", label: "Files" },
  { id: "preview", label: "Preview" },
  { id: "dev", label: "Dev" },
];

/** Terminal-palette keys editable from the per-tile ⋯ color menu. */
type TermColorKey = "background" | "foreground" | "cursor";
/** Fallbacks when neither the override nor the global theme sets a color
 *  (mirror the :root --th-term-* defaults in index.css). */
const TERM_COLOR_FALLBACK: Record<TermColorKey, string> = {
  background: "#0a0a0a",
  foreground: "#e5e5e5",
  cursor: "#10b981",
};

export interface TileProps {
  terminalId: TerminalId;
  focused: boolean;
  /** Retained for API compatibility; the xterm body no longer lives in the tile.
   *  #20 pools every terminal in a persistent, never-reparented overlay (see
   *  TerminalPool.tsx); the tile renders only its header chrome plus a ref'd
   *  placeholder the pool positions the pooled terminal over. The pool decides
   *  visibility (active tab + real placeholder rect), so this prop is unused. */
  visible?: boolean;
  /**
   * Whether THIS Tile instance owns the pool placeholder for its terminal.
   * Defaults to true. When a tile is fullscreen it is rendered TWICE — once in
   * its (hidden, covered) grid cell and once in the full-window fullscreen layer
   * (Canvas) — and both call useTerminalSlot for the same id. Exactly one must
   * register the placeholder the pool positions the xterm over, else the two
   * registrations race (last writer wins) and the xterm can land over the hidden
   * grid copy. The grid cell passes `slotActive={false}` for the fullscreen id so
   * only the fullscreen instance registers; in normal (non-fullscreen) rendering
   * this stays true and is a no-op. Only affects the Terminal tab (the only tab
   * that renders a placeholder). */
  slotActive?: boolean;
  onFocus: () => void;
  onClose: () => void;
}

/**
 * Resolve which T-Hub drop target sits under a viewport point mid-drag.
 * Precedence: a workspace tab (titlebar `data-tab-id`) or sidebar workspace row
 * (`data-th-ws-row`) — both move the tile to that workspace — win over a grid
 * tile; elementFromPoint returns the topmost element (often xterm's canvas), so
 * we walk up to the owning target with `closest`.
 *
 * A grid tile is matched by EITHER its chrome (`data-tile-id`) OR the live
 * terminal body wrapper (`data-th-pool-tile`, a sibling overlay over the body —
 * see TerminalPool). Accepting the pool wrapper is what lets a drop land anywhere
 * over a tile, not just its header: the body is covered by that wrapper, and we
 * must resolve it regardless of whether the `data-th-dragging` pointer-inert flag
 * is still set at resolution time (the drag controller clears it during cleanup).
 * Both attributes carry the terminal id, which is also the tile id.
 */
function dropTargetAt(
  x: number,
  y: number,
): { tileId: string | null; tabId: string | null } {
  // Move-to-workspace targets first: a titlebar tab, then a sidebar workspace row
  // (its data-th-ws-row value IS the tab/workspace id, so it reuses the tab path).
  const tab = resolveDropTarget(x, y, ["[data-tab-id]"]);
  if (tab) return { tileId: null, tabId: tab.value };
  const wsRow = resolveDropTarget(x, y, ["[data-th-ws-row]"]);
  if (wsRow) return { tileId: null, tabId: wsRow.value };
  // Grid tile: chrome or live-terminal body (either anchor → the terminal id).
  const tile = resolveDropTarget(x, y, ["[data-tile-id]", "[data-th-pool-tile]"]);
  if (tile) return { tileId: tile.value, tabId: null };
  return { tileId: null, tabId: null };
}

export function Tile({
  terminalId,
  focused,
  slotActive = true,
  onFocus,
  onClose,
}: TileProps) {
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
  // Lifecycle: the × KILLS this terminal's tmux session for good. The actual kill
  // is the `onClose` prop, which Canvas wires to deleteTerminal -> killTerminal
  // (and, for the fullscreen copy, also drops the fullscreen layer). We confirm
  // first only when the session looks BUSY (see `busy` below); killed immediately
  // when idle. Kept on `onClose` (not a direct store call) so the grid and
  // fullscreen close paths both run their full cleanup.
  const setDraggingTile = useWorkspace((s) => s.setDraggingTile);
  const setDropTile = useWorkspace((s) => s.setDropTile);
  const setDropTab = useWorkspace((s) => s.setDropTab);
  // Which tile is the drag source / current drop target (for visual state).
  const draggingTileId = useWorkspace((s) => s.draggingTileId);
  const dropTileId = useWorkspace((s) => s.dropTileId);
  // Themed header behavior: collapse the header to a hover-reveal hairline.
  // Subscribed so a live toggle in the editor re-renders. (The cwd is no longer
  // shown in the header — Feature 1 replaced the path with folder + Claude chip —
  // so chrome.showCwd is intentionally not read here anymore.)
  const headerOnHover = useTheme((s) => s.active.chrome.headerOnHover);
  const showTileHeader = useTheme((s) => s.active.chrome.showTileHeader);

  // BUSY detection for the kill (×) confirm gate. A tile is "busy" — so its ×
  // confirms before killing — when a managed dev server is running for it
  // (usePanels.devUrl) OR its Claude session is MID-TURN (working / needsQuestion
  // / needsPermission / waitingOnSubagents). The mid-turn status lives in the
  // supervision store keyed by CLAUDE SESSION ID; the robust tmux-session binding
  // (the statusline now stamps its owning `th_<terminalId>`) finally gives us the
  // tile -> session link the old cwd-guess lacked, so we can gate on a live turn —
  // see `tmuxSessionMidTurn`. An idle / finished session still kills immediately.
  const devUrl = usePanels((s) => s.devUrl[terminalId]);
  const claudeMidTurn = useSupervision((s) =>
    tmuxSessionMidTurn(s, sessionNameForTerminal(terminalId)),
  );
  const busy =
    (typeof devUrl === "string" && devUrl.length > 0) || claudeMidTurn;

  // Per-tile panel state (the Terminal / Files / Preview / Dev workbench). Kept
  // in usePanels — NOT workspace.ts — so this presentational state doesn't
  // contend with the workspace store. The active tab decides whether the body
  // shows the pooled terminal (terminal) or an in-tile surface (files/preview/
  // dev); fullscreen blows this one tile up to fill the window. Any unknown
  // value (e.g. a tab dropped in a past build) falls back to "terminal".
  const rawTab = usePanels((s) => s.tab[terminalId]);
  const activeTab: PanelTab =
    rawTab === "files" || rawTab === "preview" || rawTab === "dev"
      ? rawTab
      : "terminal";
  const setTab = usePanels((s) => s.setTab);
  const toggleFullscreen = usePanels((s) => s.toggleFullscreen);
  const isFullscreen = usePanels((s) => s.fullscreenId === terminalId);
  // Captain designation (captain-list): whether THIS tile is one of the pinned
  // captains, for the context-menu Pin/Unpin item. Toggling is a store action.
  const isCaptain = useCaptain((s) => s.captainIds.includes(terminalId));
  const toggleCaptain = useCaptain((s) => s.toggleCaptain);
  // Orchestrator designation (captains deck): whether THIS tile is the terminal
  // the deck's bottom input talks to, for the context-menu Mark/Unmark item.
  const isOrchestrator = useCaptain((s) => s.orchestratorId === terminalId);
  const setOrchestratorId = useCaptain((s) => s.setOrchestratorId);
  // Per-terminal color override (the ⋯ menu): the effective color comes from
  // this terminal's override first, then the global theme, then a fallback.
  const termPalette = useTheme((s) => s.active.terminal);
  const termOverride = useTheme((s) => s.termOverrides[terminalId]);
  const setTermOverride = useTheme((s) => s.setTermOverride);
  const clearTermOverride = useTheme((s) => s.clearTermOverride);
  const termFocusRing = useTheme((s) => s.termFocusRing[terminalId]);
  const setTermFocusRing = useTheme((s) => s.setTermFocusRing);
  const clearTermFocusRing = useTheme((s) => s.clearTermFocusRing);
  const themeFocusRing = useTheme((s) => s.active.chrome.focusRing);
  // Auto-continue on usage reset (the ⋯ menu): when ON, if this session runs out
  // of usage, lib/autoContinueMount waits for the window to reset and types the
  // continue command. A primitive boolean selector, so the tile only re-renders
  // when THIS terminal's opt-in flips.
  const autoContinueOn = useAutoContinue((s) => s.enabled[terminalId] === true);
  const toggleAutoContinue = useAutoContinue((s) => s.toggle);
  // The workspace (tab) this tile belongs to, and that workspace's color identity
  // (feat/workspace-colors). The tab id is derived from the live tab list, then
  // its color is looked up in the theme store. The workspace color cascades to
  // the tile's focus ring (below).
  const workspaceTabId = useWorkspace((s) =>
    tabIdForTerminal(s, terminalId),
  );
  const workspaceColor = useTheme((s) =>
    workspaceTabId ? s.workspaceColors[workspaceTabId] : undefined,
  );
  // The tile's IDENTITY color (D1), in priority order (each beats the next):
  //   1. this terminal's own override (the ⋯ menu) — most specific;
  //   2. the owning workspace's color — the per-tab identity cascade;
  //   3. none (null) — fall back to the default chrome tokens.
  // Unlike before (when only the FOCUS RING used it), this now cascades to ALL
  // of the tile's chrome — header background + border below — for EVERY tile in
  // the workspace, focused or not, and updates live as the workspace color
  // changes (both inputs are subscribed via useTheme).
  const tileColor: string | null = termFocusRing ?? workspaceColor ?? null;
  // Focused-tile ring: the identity color when present, else the global default.
  const focusRing = tileColor ?? "var(--th-focus-ring)";
  const effColor = (k: TermColorKey): string =>
    termOverride?.[k] ?? termPalette?.[k] ?? TERM_COLOR_FALLBACK[k];
  const setColor = (k: TermColorKey, value: string): void =>
    setTermOverride(terminalId, { [k]: value });
  // Expanded vs split. DEFAULT is EXPANDED (true): opening a non-terminal tab
  // fills the tile with the panel and PARKS the terminal — clean and reliable
  // (no pooled-xterm overlay competing with / covering the panel, which made
  // files render invisibly in the split). The ⇿ control switches to the SPLIT
  // (terminal beside the panel) for those who want both at once.
  const panelExpanded = usePanels((s) => s.panelExpanded[terminalId] ?? true);
  const togglePanelExpanded = usePanels((s) => s.togglePanelExpanded);
  // SPLIT divider position: the terminal half's width fraction (the panel gets
  // the rest). Persisted per tile; defaulted+clamped by the store. The divider
  // below is a pointer-drag handle that writes this live.
  const splitRatio = usePanels(
    (s) => s.splitRatio[terminalId] ?? DEFAULT_SPLIT_RATIO,
  );
  const setSplitRatio = usePanels((s) => s.setSplitRatio);
  // The split flex row, measured during a divider drag to map pointer-x → ratio.
  const splitRowRef = useRef<HTMLDivElement | null>(null);
  // Canceller for an in-flight header drag (#3): set while a drag is live, nulled
  // when it ends, and invoked from the unmount cleanup below so a tile removed
  // mid-drag (close / tab switch) can't leak window listeners or a stuck
  // `data-th-dragging` body flag.
  const dragCancelRef = useRef<PointerDragCanceller | null>(null);

  // Cosmetic "work name" (Feature 1): a free-text label the user types to say what
  // they're working on. Keyed by CWD (project path), not the terminal id, so it's
  // durable — it also shows in the sidebar Workspaces list + the project's Recent
  // row and survives relaunch/resume. Subscribed so an inline edit re-renders.
  const workName = useTheme((s) => s.workNames[info?.cwd ?? ""]);
  const setWorkName = useTheme((s) => s.setWorkName);

  const state: TerminalState = info?.state ?? "starting";

  // HEADER STATUS INDICATOR (the ring+center). Same precise derivation as the
  // sidebar (terminalVariant): a bound agent session (Claude) drives the variant
  // from its reducer status — distinct working / asking-a-question / idle, and an
  // idle Claude TUI that keeps redrawing no longer false-pulses as "working" — and
  // a session-less shell / Codex falls back to output activity (pulse while a
  // command runs, blank when quiet).
  const sessionStatus = useSupervision((s) =>
    sessionStatusForTmux(s, sessionNameForTerminal(terminalId)),
  );
  const outputActive = useActivity((s) => !!s.active[terminalId]);
  const tileVariant = terminalVariant(state, sessionStatus, outputActive);

  const cwd = info?.cwd ?? "";
  // Git facts for this tile's project (branch / worktree / dirty) — header chip.
  const git = useGitInfo(cwd);
  // Context-window fullness for the Claude session running in THIS tile. Bound
  // ROBUSTLY by tmux session name: the agent stamps each statusline with the
  // owning tmux session (`th_<id>`), and this tile looks itself up by its own
  // `th_<terminalId>` (store/sessionContext.ts) — precise even when two tiles
  // share a directory. Falls back to a cwd match when the snapshot carries no
  // session (un-upgraded agent / not under tmux). null when nothing matches —
  // then <ContextMeter> renders nothing, so non-Claude / not-yet-reported tiles
  // are unchanged.
  const contextUsedPct = useContextPctForTile(terminalId, info?.cwd);
  // Display path: strip the home prefix (`/home/<user>` -> `~`) so the header
  // shows `~/n8builds/tools` instead of the noisy `/home/natkins/n8builds/tools`.
  // The full path stays in the title tooltip.
  // The folder name shown in the header: the cwd basename (e.g. `t-hub`). Falls
  // back to the derived label when there's no cwd yet, so the header is never empty.
  const folderName = cwdBasename(cwd) || null;
  // Friendly display name (user label > derived preset·cwd > short id). Kept for
  // the drag ghost / fallbacks; the header chrome itself now shows folder+Claude.
  const label = deriveLabel({
    id: terminalId,
    label: userLabel,
    title: info?.title,
    cwd: info?.cwd,
  });
  // Which client runs in this tile — Claude / Codex / a plain shell — from the
  // workspace store (spawn command/title + any live label). Drives the header
  // client icon below. clientForTerminal reads three signals: the terminal
  // record's title, the merged `labels` entry, and the Claude-suggested title.
  // `info` + `userLabel` above subscribe to the first two; subscribe to the
  // third here too, because once a tile has an explicit rename `labels[id]`
  // stops tracking `claudeTitles[id]` (the rename wins in the merge), so without
  // this the icon could miss a late Claude title. With it, the icon re-renders
  // on any signal clientForTerminal reads and never goes stale.
  useWorkspace((s) => s.claudeTitles[terminalId]);
  const client = clientForTerminal(terminalId);
  const isSelfDragging = draggingTileId === terminalId;
  const isDropTarget = dropTileId === terminalId && draggingTileId !== terminalId;

  // Whether the kill confirm is up for this tile. Only shown when the session is
  // BUSY (a dev server running) — an idle session is killed immediately. Once up,
  // the kill runs on confirm (button / Enter); cancel/Esc/backdrop dismiss it.
  const [confirmKill, setConfirmKill] = useState(false);
  // Right-click context menu position (null = closed). Right-clicking the header
  // opens a single "Kill session" action at the pointer.
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number } | null>(null);
  // Per-terminal color popover position (null = closed). Right-aligned under the
  // ⋯ button so it never spills off the tile's right edge.
  const [colorMenu, setColorMenu] = useState<{
    right: number;
    top: number;
  } | null>(null);
  // Inline work-name editor (Feature 1): null = display mode; a string = the live
  // draft while editing. Enter commits, Esc cancels. Seeded from the saved name.
  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const startNameEdit = () => setNameDraft(workName ?? "");
  const commitName = () => {
    // Keyed by cwd: a blank draft clears it (setWorkName handles that).
    if (nameDraft !== null && cwd) setWorkName(cwd, nameDraft);
    setNameDraft(null);
  };

  // The ONE close path for this tile's × and the context-menu action: kill the
  // session, but confirm first if it looks busy. Idle -> kill now; busy -> ask.
  const requestKill = () => {
    if (busy) setConfirmKill(true);
    else onClose();
  };

  // Abort any in-flight header drag if this tile unmounts mid-gesture (#3): the
  // canceller runs the controller's full cleanup (listeners + grabbing cursor +
  // the owned `data-th-dragging` flag) and fires onEnd(committed=false), so a
  // tile closed/moved while being dragged never leaves the app stuck pointer-inert.
  useEffect(() => {
    return () => {
      dragCancelRef.current?.();
      dragCancelRef.current = null;
    };
  }, []);

  // --- SPLIT divider drag: resize the terminal|panel halves ---
  // Pointer-based (not HTML5 DnD) like every other T-Hub drag, with pointer
  // CAPTURE so the gesture keeps tracking even as it crosses the xterm canvas /
  // panel (whose own pointer handlers would otherwise steal events). On each move
  // we map the pointer's x within the split row to the terminal-half fraction and
  // write it live; the store clamps it to a sane min/max and persists it.
  const onDividerPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0) return; // primary button only
    e.preventDefault();
    e.stopPropagation(); // don't bubble to the tile/header (focus/move) handlers
    const row = splitRowRef.current;
    if (!row) return;
    const handle = e.currentTarget as HTMLElement;
    try {
      handle.setPointerCapture(e.pointerId);
    } catch {
      /* capture is best-effort; the move handler still works without it */
    }
    // Suppress text selection + show the resize cursor for the whole gesture.
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";

    const applyFromClientX = (clientX: number) => {
      const rect = row.getBoundingClientRect();
      if (rect.width <= 0) return;
      const frac = (clientX - rect.left) / rect.width;
      setSplitRatio(terminalId, clampSplitRatio(frac));
    };

    const onMove = (ev: PointerEvent) => applyFromClientX(ev.clientX);
    const cleanup = () => {
      window.removeEventListener("pointermove", onMove, true);
      window.removeEventListener("pointerup", onUp, true);
      window.removeEventListener("pointercancel", onUp, true);
      document.body.style.removeProperty("user-select");
      document.body.style.removeProperty("cursor");
      try {
        handle.releasePointerCapture(e.pointerId);
      } catch {
        /* already released */
      }
    };
    const onUp = () => cleanup();

    window.addEventListener("pointermove", onMove, true);
    window.addEventListener("pointerup", onUp, true);
    window.addEventListener("pointercancel", onUp, true);
  };

  // --- Drag source (the header), pointer-based ---
  const onHeaderPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0) return; // primary (left) button only
    onFocus(); // pressing the header selects the tile right away
    const sourceId = terminalId;
    let ghost: DragGhost | null = null;
    // Stash the canceller so an unmount mid-drag (#3) can abort the gesture —
    // clearing the listeners, the body drag flag, and the drag state — rather
    // than leaking a stuck `data-th-dragging` that leaves every terminal
    // pointer-inert. The drag controller now OWNS that body flag
    // (manageBodyDragFlag), so this handler no longer sets/clears it by hand.
    dragCancelRef.current = startPointerDrag(e.clientX, e.clientY, {
      manageBodyDragFlag: true,
      onBegin: () => {
        setDraggingTile(sourceId);
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
        dragCancelRef.current = null;
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
            ? `color-mix(in srgb, ${focusRing} 55%, var(--th-border))`
            : tileColor
              ? // Unfocused but in a colored workspace: a subtle identity tint
                // (D1) so the color cascades to every tile, not just the focused.
                `color-mix(in srgb, ${tileColor} 38%, var(--th-border))`
              : "var(--th-border)",
        boxShadow: isDropTarget
          ? "inset 0 0 0 2px var(--th-accent)"
          : focused
            ? `0 0 0 1px color-mix(in srgb, ${focusRing} 40%, transparent), 0 0 16px -4px color-mix(in srgb, ${focusRing} 60%, transparent)`
            : "none",
      }}
    >
      {/* Header. Pressing anywhere here focuses the tile; it is also the pointer
          drag handle for move. Height + colors + visibility are themed; the
          `th-tile-header` class lets index.css drive the hover-reveal mode. */}
      <div
        onPointerDown={onHeaderPointerDown}
        // Right-click the header → a Close / Delete context menu at the pointer
        // (focus the tile first so the action targets it).
        onContextMenu={(e) => {
          e.preventDefault();
          onFocus();
          setCtxMenu({ x: e.clientX, y: e.clientY });
        }}
        // Height / display / hover-reveal are driven from index.css (off the
        // `th-tile-header` class + the parent's data-header-hover) so the
        // stylesheet's hover rule can override the height — an inline height
        // would win by specificity and break the reveal. Only the per-instance
        // colors/font live inline here.
        className="th-tile-header relative flex shrink-0 cursor-grab touch-none select-none items-center gap-2 border-b px-2 active:cursor-grabbing"
        style={{
          // Header chrome carries the workspace/identity color too (D1): a
          // subtle wash over the header bg + a stronger bottom border, so the
          // whole tile — not just its focus ring — reads as part of its
          // workspace. Falls back to the plain tokens when there's no color.
          backgroundColor: tileColor
            ? `color-mix(in srgb, ${tileColor} 14%, var(--th-header-bg))`
            : "var(--th-header-bg)",
          borderColor: tileColor
            ? `color-mix(in srgb, ${tileColor} 30%, var(--th-border))`
            : "var(--th-border)",
          fontSize: "var(--th-font-size)",
        }}
        title={cwd}
      >
        {/* Client identity glyph (A1/A2/A3): which agent runs in this tile,
            pinned as far LEFT as possible — the FIRST item in the header row,
            ahead of the lifecycle dot, folder name and other chrome. The old
            always-on "Claude" TEXT chip over-claimed — a plain shell isn't
            Claude — so we DETECT the client (clientForTerminal) and show JUST
            the matching icon, no word and no circular badge: Claude's clay
            spark, Codex's blue `>_` mark, or nothing for a plain shell. The
            tooltip names it. */}
        {client !== "shell" && (
          <span
            className="inline-flex shrink-0 items-center justify-center leading-none"
            title={client === "claude" ? "Claude" : "Codex"}
          >
            {client === "claude" ? (
              // The real Claude brand glyph, tinted Claude's brand clay
              // (#D97757). No badge — just the bare glyph on the header bg.
              <ClaudeIcon
                size="1.1em"
                className="shrink-0"
                style={{ color: "#D97757" }}
                title="Claude"
              />
            ) : (
              <CodexIcon size="1.1em" className="shrink-0" title="Codex" />
            )}
          </span>
        )}
        {/* Session status: the shared ring+center indicator. It now reflects the
            agent's WORK (pulses while mid-turn / streaming) rather than only the
            xterm lifecycle — matching the sidebar row. Hover for the exact xterm
            state. Kept small/low-key (#5) so it doesn't read as a "selected"
            marker. */}
        {/* Captain marker (captain-overlay): a small anchor on the pinned
            captain's header, so the summon target is identifiable at a glance. */}
        {isCaptain && (
          <span
            className="inline-flex shrink-0 items-center leading-none"
            style={{ color: "var(--th-accent)" }}
            title="Pinned captain - summon with Ctrl+B C"
          >
            <Anchor size="0.95em" aria-label="Captain session" />
          </span>
        )}
        <StatusIndicator
          variant={tileVariant}
          size={9}
          title={
            tileVariant === "working"
              ? `Working… (terminal: ${state})`
              : `Terminal state: ${state}`
          }
          className="shrink-0"
        />
        {/* Folder name (Feature 1). The path display is gone — the folder
            basename is enough to place the work. The full cwd stays in the
            header tooltip (the header's `title={cwd}`). */}
        {folderName && (
          <span
            className="shrink-0 truncate"
            style={{ color: "var(--th-fg)", fontSize: "1.05em" }}
            title={cwd || undefined}
          >
            {folderName}
          </span>
        )}

        {/* Current worktree / branch, PROMINENT in the title line (not a muted side
            chip) — you want to see at a glance which worktree a terminal is on. Shows
            the git branch with a branch icon, a "wt" badge when it's a linked
            worktree, and a dot when the tree is dirty. Renders nothing outside a repo.
            The folder name (a worktree's dir is `<repo>-worktrees/<sanitized-branch>`)
            can differ from the real branch (`feat/x` → dir `feat-x`), so showing the
            actual branch here is the accurate signal. */}
        {git?.isRepo && git.branch && (
          <span
            className="flex shrink-0 items-center gap-1 truncate"
            style={{ color: "var(--th-fg)", fontSize: "1.02em" }}
            title={`${git.isLinkedWorktree ? "worktree" : "branch"}: ${git.branch}${
              git.dirtyCount > 0
                ? ` · ${git.dirtyCount} uncommitted change${
                    git.dirtyCount === 1 ? "" : "s"
                  }`
                : " · clean"
            }`}
          >
            <GitBranch size="0.9em" aria-hidden style={{ opacity: 0.7 }} />
            <span className="max-w-[11rem] truncate">{git.branch}</span>
            {git.isLinkedWorktree && (
              <span
                className="rounded px-1 text-[0.7em] uppercase leading-none"
                style={{
                  backgroundColor: "var(--th-tile-bg)",
                  color: "var(--th-fg-muted)",
                }}
              >
                wt
              </span>
            )}
            {git.dirtyCount > 0 && (
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ backgroundColor: "#eab308" }}
                aria-hidden
              />
            )}
          </span>
        )}

        {/* Editable "what are you working on" name (Feature 1). Click to edit
            inline; Enter commits, Esc cancels. Persisted per-PROJECT (theme store
            workNames, keyed by cwd — so it also shows in the sidebar + Recent).
            Stops the header drag on pointer-down. Kept COMPACT (sized to content
            with a sensible cap) rather than spanning the full tile width, so the
            right-aligned view-tab bar has room. */}
        {nameDraft !== null ? (
          <input
            autoFocus
            value={nameDraft}
            onChange={(e) => setNameDraft(e.target.value)}
            onBlur={commitName}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") commitName();
              else if (e.key === "Escape") setNameDraft(null);
            }}
            placeholder="name this work…"
            spellCheck={false}
            className="w-40 max-w-[40%] shrink rounded bg-transparent px-1 py-0.5 outline-none"
            style={{
              color: "var(--th-fg)",
              border: `1px solid ${focusRing}`,
            }}
          />
        ) : (
          <button
            type="button"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onFocus();
              startNameEdit();
            }}
            className="max-w-[40%] shrink truncate rounded px-1 py-0.5 text-left hover:bg-neutral-800/50"
            style={{
              color: workName ? "var(--th-fg)" : "var(--th-fg-muted)",
              fontStyle: workName ? undefined : "italic",
            }}
            title={workName ? "Click to rename this work" : "Name what you're working on"}
          >
            {workName ?? "name this work…"}
          </button>
        )}

        {/* (The branch/worktree now renders PROMINENTLY in the title line above,
            next to the folder name — the old muted side-chip was folded into it.) */}

        {/* Flexible spacer: pushes the view-tab bar + controls to the RIGHT edge
            of the header (the tabs used to be absolutely centered). */}
        <div className="min-w-0 flex-1" />

        {/* Context-window meter: how full THIS tile's Claude session context is.
            Shown ONLY when the tile is actually running Claude — the cwd-matched
            reading can otherwise leak onto a plain shell that shares a directory
            with a (past) Claude session, so gate on the client being claude. */}
        <ContextMeter usedPct={client === "claude" ? contextUsedPct : null} />

        {/* Per-tile view switcher: Terminal / Files / Preview. Clicking a tab
            sets THIS tile's usePanels tab; the body (below) swaps to that surface
            and the terminal pool re-syncs (it subscribes to the tab) so the
            pooled xterm is shown only on the Terminal tab and parked otherwise.
            pointerDown is stopped so a tab click doesn't start the header's
            drag-to-move gesture.

            RIGHT-ALIGNED: an in-flow flex item that the spacer above pushes to
            the right edge of the header (it used to be absolutely centered). It
            sits just left of the ⋯ / ⤢ / × controls. */}
        <div
          className="z-10 flex shrink-0 items-center rounded-full border p-0.5"
          style={{
            backgroundColor: "var(--th-app-bg)",
            borderColor: "var(--th-border)",
          }}
          onPointerDown={(e) => e.stopPropagation()}
        >
          {PANEL_TABS.map((t) => {
            const selected = activeTab === t.id;
            return (
              <button
                key={t.id}
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  // Focus the tile so any view scoped to the focused terminal
                  // (e.g. cwd-derived chrome) follows when you switch its tab.
                  onFocus();
                  setTab(terminalId, t.id);
                }}
                className="rounded-full px-2.5 py-1.5 text-[0.9em] leading-none transition-colors"
                style={{
                  color: selected ? "var(--th-fg)" : "var(--th-fg-muted)",
                  backgroundColor: selected
                    ? "color-mix(in srgb, var(--th-accent) 28%, var(--th-tile-bg))"
                    : "transparent",
                  fontWeight: selected ? 600 : 400,
                }}
                title={`${t.label} view`}
                aria-pressed={selected}
              >
                {t.label}
              </button>
            );
          })}
        </div>

        {/* ⟳ refresh: RE-FIT this terminal to its current size + repaint. The
            manual recovery for a tile grown from a small corner to full that didn't
            reflow on its own (also reachable by right-clicking the header). */}
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onFocus();
            refreshTerminal(terminalId);
          }}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title="Refresh terminal (re-fit + repaint)"
          aria-label="Refresh terminal"
        >
          ⟳
        </button>

        {/* ⋯ menu: per-terminal color overrides so you can tell this terminal
            apart from the rest. Opens a small popover anchored under the button;
            edits apply live and persist (see store/theme termOverrides). */}
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onFocus();
            const r = e.currentTarget.getBoundingClientRect();
            setColorMenu((m) =>
              m ? null : { right: window.innerWidth - r.right, top: r.bottom + 4 },
            );
          }}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title="Terminal colors"
          aria-label="Terminal colors"
          aria-haspopup="menu"
          aria-expanded={colorMenu != null}
        >
          ⋯
        </button>

        {/* Fullscreen toggle (⤢): blow THIS tile up to fill the window; other
            tiles keep running underneath. Toggling (or Esc — handled in Canvas)
            returns to the grid. Kept next to the × so the two chrome controls
            sit together. */}
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onFocus();
            toggleFullscreen(terminalId);
          }}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title={isFullscreen ? "Exit fullscreen (Esc)" : "Fullscreen this tile"}
          aria-label={isFullscreen ? "Exit fullscreen" : "Fullscreen tile"}
          aria-pressed={isFullscreen}
        >
          {isFullscreen ? (
            // Collapse glyph (arrows pointing inward) when already fullscreen.
            <svg
              viewBox="0 0 16 16"
              width="0.9em"
              height="0.9em"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.3"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden
            >
              <path d="M6 2.5v3.5H2.5M10 2.5v3.5h3.5M6 13.5V10H2.5M10 13.5V10h3.5" />
            </svg>
          ) : (
            // Expand glyph (arrows pointing outward) for the enter-fullscreen state.
            <svg
              viewBox="0 0 16 16"
              width="0.9em"
              height="0.9em"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.3"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden
            >
              <path d="M6 2.5H2.5V6M10 2.5h3.5V6M6 13.5H2.5V10M10 13.5h3.5V10" />
            </svg>
          )}
        </button>

        {/* ONE lifecycle control: × KILLS this terminal's tmux session. Idle ->
            kill now; busy (a dev server running) -> confirm first (requestKill).
            Durable Claude session history means the conversation is still
            recoverable from Recent afterward, so there's no separate "detach"
            affordance anymore. */}
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            requestKill();
          }}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title={
            busy
              ? "Kill session (looks busy — asks first)"
              : "Kill session (resume later from Recent)"
          }
          aria-label="Kill session"
        >
          ×
        </button>
      </div>

      {/* Kill confirm — shown ONLY when the session looks busy (requestKill).
          An idle kill skips this entirely. */}
      <ConfirmDialog
        open={confirmKill}
        title="Kill this session?"
        body={
          <>
            This session looks busy (Claude is working / a dev server is running).
            Killing the tmux session{" "}
            <span className="font-mono" style={{ color: "var(--th-fg)" }}>
              {terminalId}
            </span>{" "}
            ends everything running in it. You can resume the conversation later
            from Recent.
          </>
        }
        confirmLabel="Kill it anyway"
        onConfirm={() => {
          setConfirmKill(false);
          onClose();
        }}
        onCancel={() => setConfirmKill(false)}
      />

      {/* Right-click context menu: a single "Kill session" action that runs the
          same busy-gated kill the × does (confirm only if busy). */}
      {ctxMenu && (
        <>
          <div
            className="fixed inset-0 z-40"
            onPointerDown={() => setCtxMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault();
              setCtxMenu(null);
            }}
          />
          <div
            className="fixed z-50 min-w-[200px] overflow-hidden rounded-md border shadow-2xl"
            style={{
              left: ctxMenu.x,
              top: ctxMenu.y,
              // Solid surface (not --th-header-bg's alpha) so this floating menu
              // never bleeds the terminal canvas through — see the ⋯ popover note.
              backgroundColor: "var(--th-tile-bg)",
              borderColor: "var(--th-border)",
              color: "var(--th-fg)",
              fontFamily: "var(--th-font)",
            }}
            onPointerDown={(e) => e.stopPropagation()}
          >
            <CtxItem
              label="Refresh terminal"
              hint="Re-fit to the current size + repaint — fixes a tile that didn't reflow after growing"
              onClick={() => {
                setCtxMenu(null);
                refreshTerminal(terminalId);
              }}
            />
            <CtxItem
              label={isCaptain ? "Unpin captain" : "Pin as captain"}
              hint={
                isCaptain
                  ? "This is a pinned captain - unpin to drop it from the summon list"
                  : "Add this session to the captains the overlay summons (Ctrl+B C)"
              }
              onClick={() => {
                setCtxMenu(null);
                toggleCaptain(terminalId);
              }}
            />
            <CtxItem
              label={isOrchestrator ? "Unmark orchestrator" : "Mark as orchestrator"}
              hint={
                isOrchestrator
                  ? "This session receives the captains-deck bottom input - unmark to clear it"
                  : "Make this session the orchestrator the captains-deck input talks to"
              }
              onClick={() => {
                setCtxMenu(null);
                setOrchestratorId(isOrchestrator ? null : terminalId);
              }}
            />
            <CtxItem
              label="Kill session"
              hint="Ends the tmux session — resume later from Recent (asks first if busy)"
              danger
              onClick={() => {
                setCtxMenu(null);
                requestKill();
              }}
            />
          </div>
        </>
      )}

      {/* Per-terminal color popover (⋯). Mirrors the context-menu pattern: a
          full-window backdrop to dismiss, plus a small fixed panel anchored under
          the button. Editing a swatch writes straight to the store, which
          live-applies to THIS terminal's xterm (store/theme termOverrides). */}
      {colorMenu && (
        <>
          <div
            className="fixed inset-0 z-40"
            onPointerDown={() => setColorMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault();
              setColorMenu(null);
            }}
          />
          <div
            className="fixed z-50 w-[210px] overflow-hidden rounded-md border p-2 shadow-2xl"
            style={{
              right: colorMenu.right,
              top: colorMenu.top,
              // FULLY OPAQUE surface (the header-bg token carries an alpha in
              // some themes, which let the terminal bleed through). Use the solid
              // tile-bg token so the color popover never shows transparency.
              backgroundColor: "var(--th-tile-bg)",
              borderColor: "var(--th-border)",
              color: "var(--th-fg)",
              fontFamily: "var(--th-font)",
              fontSize: "var(--th-font-size)",
            }}
            onPointerDown={(e) => e.stopPropagation()}
          >
            <div
              className="mb-1.5 px-0.5 text-xs font-semibold uppercase tracking-wide"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Terminal colors
            </div>
            {(
              [
                ["Background", "background"],
                ["Foreground", "foreground"],
                ["Cursor", "cursor"],
              ] as [string, TermColorKey][]
            ).map(([label, key]) => (
              <label
                key={key}
                className="flex items-center justify-between gap-2 px-0.5 py-1"
              >
                <span>{label}</span>
                <input
                  type="color"
                  value={effColor(key)}
                  onChange={(e) => setColor(key, e.target.value)}
                  className="h-6 w-9 shrink-0 cursor-pointer rounded bg-transparent p-0"
                  title={`${label} color`}
                />
              </label>
            ))}
            <label className="flex items-center justify-between gap-2 px-0.5 py-1">
              <span>Focus ring</span>
              <input
                type="color"
                value={termFocusRing ?? themeFocusRing}
                onChange={(e) => setTermFocusRing(terminalId, e.target.value)}
                className="h-6 w-9 shrink-0 cursor-pointer rounded bg-transparent p-0"
                title="Focus-ring color when this terminal is focused"
              />
            </label>
            <button
              type="button"
              onClick={() => {
                clearTermOverride(terminalId);
                clearTermFocusRing(terminalId);
                setColorMenu(null);
              }}
              disabled={!termOverride && !termFocusRing}
              className="mt-1.5 w-full rounded border px-2 py-1 text-xs hover:bg-neutral-800 disabled:opacity-40"
              style={{
                borderColor: "var(--th-border)",
                color: "var(--th-fg-muted)",
              }}
              title="Clear this terminal's colors and follow the global theme"
            >
              Reset to theme
            </button>

            {/* Auto-continue on usage reset (per-terminal opt-in). When this
                session runs out of usage, T-Hub waits for the limit window to
                reset and types the continue command so the agent resumes. */}
            <div
              className="my-2 border-t"
              style={{ borderColor: "var(--th-border)" }}
            />
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={autoContinueOn}
              onClick={() => toggleAutoContinue(terminalId)}
              className="flex w-full items-center justify-between gap-2 rounded px-0.5 py-1 text-xs hover:bg-neutral-800/40"
              title="When this session runs out of usage, wait for the limit to reset and type the continue command automatically"
            >
              <span className="text-left leading-tight">
                Auto-continue on usage reset
              </span>
              <span
                className="ml-2 inline-flex h-4 w-7 shrink-0 items-center rounded-full px-0.5 transition-colors"
                style={{
                  backgroundColor: autoContinueOn
                    ? "var(--th-accent)"
                    : "var(--th-border)",
                }}
                aria-hidden
              >
                <span
                  className="h-3 w-3 rounded-full transition-transform"
                  style={{
                    backgroundColor: "#fff",
                    transform: autoContinueOn
                      ? "translateX(12px)"
                      : "translateX(0)",
                  }}
                />
              </span>
            </button>
          </div>
        </>
      )}

      {/* Body. The pooled xterm is rendered ONCE in the overlay (TerminalPool)
          and positioned over the placeholder DIV below; whichever placeholder is
          mounted (full tile on the Terminal tab, or just the terminal HALF in a
          split) is where the xterm lands + sizes itself. The non-terminal tabs
          render their surface (Files/Preview/Dev) via <TilePanel>:
            - SPLIT (default for a non-terminal tab): terminal half + panel half
              side by side; the pool keeps showing the xterm in its half.
            - EXPANDED: the panel fills the tile; no placeholder is mounted, so
              the pool parks the xterm (TerminalPool.shouldShow keys off
              panelExpanded). */}
      {isFullscreen && !slotActive ? (
        // COVERED grid copy of a fullscreen tile (fullscreen layer renders the
        // visible copy): empty body, no placeholder (slotActive=false), no panel.
        <div className="min-h-0 flex-1 overflow-hidden" />
      ) : activeTab === "terminal" ? (
        // Terminal-only: the placeholder fills the whole body. slotActive guards
        // the fullscreen double-render (the hidden grid copy doesn't re-register).
        <div
          ref={slotActive ? slotRef : undefined}
          className="min-h-0 flex-1 overflow-hidden"
        />
      ) : panelExpanded ? (
        // Panel EXPANDED: it fills the tile; the pool parks the terminal.
        <div className="min-h-0 flex-1 overflow-hidden">
          <PanelPane
            terminalId={terminalId}
            cwd={cwd}
            tab={activeTab}
            expanded
            onClose={() => setTab(terminalId, "terminal")}
            onToggleExpand={() => togglePanelExpanded(terminalId)}
          />
        </div>
      ) : (
        // SPLIT: terminal half (placeholder) + DRAGGABLE divider + panel half,
        // side by side. The terminal half's width is `splitRatio` of the row and
        // the panel takes the rest; the divider between them is a pointer-drag
        // handle (onDividerPointerDown) that rewrites the ratio live, clamped +
        // persisted per tile. flex-basis (not flex-1) so the ratio is honored.
        <div ref={splitRowRef} className="flex min-h-0 flex-1">
          <div
            ref={slotActive ? slotRef : undefined}
            className="min-h-0 min-w-0 shrink-0 grow-0 overflow-hidden"
            style={{ flexBasis: `${splitRatio * 100}%` }}
          />
          {/* Divider: a thin themed bar with a wider invisible hit area, so it's
              easy to grab without a fat visible seam. col-resize cursor signals
              draggability; touch-none keeps touch/pen from scrolling mid-drag. */}
          <div
            onPointerDown={onDividerPointerDown}
            // Double-click snaps back to an even split (a common resizer nicety).
            onDoubleClick={(e) => {
              e.stopPropagation();
              setSplitRatio(terminalId, DEFAULT_SPLIT_RATIO);
            }}
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize terminal and panel"
            title="Drag to resize · double-click to reset"
            className="relative z-10 w-px shrink-0 cursor-col-resize touch-none select-none"
            style={{ backgroundColor: "var(--th-border)" }}
          >
            {/* Invisible padded grab zone straddling the 1px seam. */}
            <span className="absolute inset-y-0 -left-1.5 -right-1.5 block" />
          </div>
          <div
            className="min-h-0 min-w-0 flex-1 overflow-hidden"
            style={{ flexBasis: `${(1 - splitRatio) * 100}%` }}
          >
            <PanelPane
              terminalId={terminalId}
              cwd={cwd}
              tab={activeTab}
              expanded={false}
              onClose={() => setTab(terminalId, "terminal")}
              onToggleExpand={() => togglePanelExpanded(terminalId)}
            />
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * The panel half of a split (or the full panel when expanded): a thin toolbar
 * (the view name + expand/collapse + close) over the TilePanel surface. Close
 * returns the tile to the terminal; expand toggles fill-the-tile vs split.
 */
function PanelPane({
  terminalId,
  cwd,
  tab,
  expanded,
  onClose,
  onToggleExpand,
}: {
  terminalId: TerminalId;
  cwd: string;
  tab: Exclude<PanelTab, "terminal">;
  expanded: boolean;
  onClose: () => void;
  onToggleExpand: () => void;
}) {
  const title = tab.charAt(0).toUpperCase() + tab.slice(1);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-center gap-2 border-b px-2 py-1"
        style={{
          backgroundColor: "var(--th-header-bg)",
          borderColor: "var(--th-border)",
          fontSize: "var(--th-font-size)",
        }}
      >
        <span className="min-w-0 flex-1 truncate" style={{ color: "var(--th-fg-muted)" }}>
          {title}
        </span>
        <button
          type="button"
          onClick={onToggleExpand}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title={expanded ? "Back to split" : "Expand panel (hide terminal)"}
          aria-label={expanded ? "Back to split" : "Expand panel"}
        >
          {expanded ? "⇔" : "⇿"}
        </button>
        <button
          type="button"
          onClick={onClose}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: "var(--th-fg-muted)" }}
          title="Close panel (back to terminal)"
          aria-label="Close panel"
        >
          ×
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        {/* ALWAYS compact: a tile cell (split half OR full tile in a multi-tile
            grid) is never wide enough for FilePanel's 288px side-by-side rail —
            that cramped the tree to nothing. Compact = stacked tree/reader. */}
        <TilePanel terminalId={terminalId} cwd={cwd} tab={tab} compact />
      </div>
    </div>
  );
}

/** Final path segment of a (possibly trailing-slashed) cwd, or "" when none.
 *  POSIX and Windows separators both split, so a WSL or native path yields a
 *  basename; the literal home tilde collapses to "". Used for the header folder
 *  name (Feature 1). */
function cwdBasename(cwd: string): string {
  if (!cwd) return "";
  const parts = cwd.replace(/[/\\]+$/, "").split(/[/\\]+/);
  const last = parts[parts.length - 1] ?? "";
  return last === "~" ? "" : last;
}

/** One row in the tile right-click context menu. */
function CtxItem({
  label,
  hint,
  danger,
  onClick,
}: {
  label: string;
  hint: string;
  danger?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full flex-col items-start gap-0.5 px-3 py-2 text-left transition-colors hover:bg-neutral-700/30"
    >
      <span
        className="text-sm"
        style={{ color: danger ? "var(--th-dot-error)" : "var(--th-fg)" }}
      >
        {label}
      </span>
      <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
        {hint}
      </span>
    </button>
  );
}
