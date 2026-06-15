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
import { useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { TerminalId, TerminalState } from "../ipc/types";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { useTheme } from "../store/theme";
import { usePanels, type PanelTab } from "../store/panels";
import { useTerminalSlot } from "./TerminalPool";
import { TilePanel } from "./TilePanel";
import { startPointerDrag } from "../lib/pointerDrag";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { ConfirmDialog } from "./ConfirmDialog";

/** The tile-header tab bar order + labels. Terminal is the default view. */
const PANEL_TABS: { id: PanelTab; label: string }[] = [
  { id: "terminal", label: "Terminal" },
  { id: "files", label: "Files" },
  { id: "preview", label: "Preview" },
  { id: "dev", label: "Dev" },
];

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
  // Themed header behavior: hide the cwd, and/or collapse the header to a
  // hover-reveal hairline. Subscribed so a live toggle in the editor re-renders.
  const showCwd = useTheme((s) => s.active.chrome.showCwd);
  const headerOnHover = useTheme((s) => s.active.chrome.headerOnHover);
  const showTileHeader = useTheme((s) => s.active.chrome.showTileHeader);

  // BUSY detection for the kill (×) confirm gate. A tile is "busy" when there is
  // a managed dev server running for it — usePanels.devUrl[id] is a non-null URL
  // (set by the Dev runner, cleared when the server stops). Subscribed so the
  // gate is always current.
  //
  // NOTE (kill confirm scope): the spec also wants an ACTIVE Claude turn
  // (working / needsQuestion / needsPermission / waitingOnSubagents) to count as
  // busy. That status lives in the supervision store keyed by CLAUDE SESSION ID,
  // and the frontend has no reliable terminal-id -> session-id bridge (the only
  // correlation, in workspace.ts, is a best-effort cwd match driven off the
  // agent://title EVENT payload, and the stored statuses carry no cwd at all). A
  // cwd guess here could mis-gate the kill on the wrong tile, so per the task's
  // documented fallback we treat busy = (dev server running) only and kill idle
  // sessions without a dialog. When a real terminal<->session mapping lands,
  // fold the working/waiting statuses into `busy` here.
  const devUrl = usePanels((s) => s.devUrl[terminalId]);
  const busy = typeof devUrl === "string" && devUrl.length > 0;

  // Per-tile panel state (the Terminal / Files / Preview / Dev workbench). Kept
  // in usePanels — NOT workspace.ts — so this presentational state doesn't
  // contend with the workspace store. The active tab decides whether the body
  // shows the pooled terminal (terminal) or an in-tile surface (files/preview/
  // dev); fullscreen blows this one tile up to fill the window.
  const activeTab = usePanels((s) => s.tab[terminalId] ?? "terminal");
  const setTab = usePanels((s) => s.setTab);
  const toggleFullscreen = usePanels((s) => s.toggleFullscreen);
  const isFullscreen = usePanels((s) => s.fullscreenId === terminalId);
  // Expanded vs split. DEFAULT is EXPANDED (true): opening a non-terminal tab
  // fills the tile with the panel and PARKS the terminal — clean and reliable
  // (no pooled-xterm overlay competing with / covering the panel, which made
  // files render invisibly in the split). The ⇿ control switches to the SPLIT
  // (terminal beside the panel) for those who want both at once.
  const panelExpanded = usePanels((s) => s.panelExpanded[terminalId] ?? true);
  const togglePanelExpanded = usePanels((s) => s.togglePanelExpanded);

  const state: TerminalState = info?.state ?? "starting";
  const cwd = info?.cwd ?? "";
  // Display path: strip the home prefix (`/home/<user>` -> `~`) so the header
  // shows `~/n8builds/tools` instead of the noisy `/home/natkins/n8builds/tools`.
  // The full path stays in the title tooltip.
  const shortCwd = shortenHomePath(cwd);
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

  // Whether the kill confirm is up for this tile. Only shown when the session is
  // BUSY (a dev server running) — an idle session is killed immediately. Once up,
  // the kill runs on confirm (button / Enter); cancel/Esc/backdrop dismiss it.
  const [confirmKill, setConfirmKill] = useState(false);
  // Right-click context menu position (null = closed). Right-clicking the header
  // opens a single "Kill session" action at the pointer.
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number } | null>(null);

  // The ONE close path for this tile's × and the context-menu action: kill the
  // session, but confirm first if it looks busy. Idle -> kill now; busy -> ask.
  const requestKill = () => {
    if (busy) setConfirmKill(true);
    else onClose();
  };

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
            title={cwd}
          >
            {shortCwd}
          </span>
        )}
        {(!showCwd || !cwd) && <span className="flex-1" />}

        {/* Per-tile view switcher: Terminal / Files / Preview / Dev. Clicking a
            tab sets THIS tile's usePanels tab; the body (below) swaps to that
            surface and the terminal pool re-syncs (it subscribes to the tab) so
            the pooled xterm is shown only on the Terminal tab and parked
            otherwise. pointerDown is stopped so a tab click doesn't start the
            header's drag-to-move gesture. */}
        <div
          className="flex shrink-0 items-center gap-0.5"
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
                className="rounded px-1.5 py-0.5 text-[0.85em] leading-none transition-colors"
                style={{
                  color: selected ? "var(--th-fg)" : "var(--th-fg-muted)",
                  backgroundColor: selected
                    ? "color-mix(in srgb, var(--th-accent) 22%, transparent)"
                    : "transparent",
                }}
                title={`${t.label} view`}
                aria-pressed={selected}
              >
                {t.label}
              </button>
            );
          })}
        </div>

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
              backgroundColor: "var(--th-header-bg)",
              borderColor: "var(--th-border)",
              color: "var(--th-fg)",
              fontFamily: "var(--th-font)",
            }}
            onPointerDown={(e) => e.stopPropagation()}
          >
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
        // SPLIT: terminal half (placeholder) + panel half, side by side.
        <div className="flex min-h-0 flex-1">
          <div
            ref={slotActive ? slotRef : undefined}
            className="min-h-0 min-w-0 flex-1 overflow-hidden"
          />
          <div
            className="min-h-0 w-1/2 max-w-[60%] shrink-0 overflow-hidden border-l"
            style={{ borderColor: "var(--th-border)" }}
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
        {/* Split (not expanded) is narrow -> FilePanel uses its compact layout. */}
        <TilePanel terminalId={terminalId} cwd={cwd} tab={tab} compact={!expanded} />
      </div>
    </div>
  );
}

/** Shorten a WSL/POSIX path for display: collapse `/home/<user>` to `~` so the
 *  header reads `~/n8builds/tools` instead of `/home/natkins/n8builds/tools`.
 *  Leaves non-home paths untouched. The full path is kept in tooltips. */
function shortenHomePath(p: string): string {
  if (!p) return p;
  const m = p.match(/^\/home\/[^/]+(\/.*)?$/);
  if (m) return "~" + (m[1] ?? "");
  // Windows-style home, just in case.
  const w = p.match(/^[A-Za-z]:[\\/]Users[\\/][^\\/]+([\\/].*)?$/);
  if (w) return "~" + (w[1] ?? "");
  return p;
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
