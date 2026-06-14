// A tile is a thin header (status dot, title, cwd, close) above an EMPTY body
// placeholder. It fills its grid cell and surfaces selection as a subtle theme-
// accent glow. The terminal's xterm body is NOT a child of the tile: #20 renders
// each terminal once in a persistent pool overlay (TerminalPool.tsx) and
// positions it over this tile's placeholder, so moving/resizing the tile only
// repositions the pooled terminal — it is never remounted/reattached (no flash).
// Pressing the header focuses the tile. The header carries TWO lifecycle
// affordances (feat/lifecycle): the × DETACHES the tile (closeTerminal) while
// KEEPING the tmux session alive so it can be re-adopted later — the default,
// non-destructive close. A separate trash control DELETES the session for good
// (killTerminal), gated behind a themed confirm dialog; shift-clicking the × is
// a shortcut to that same confirmed delete.
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
import { useShiftHeld } from "../lib/useShiftHeld";

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
  // Lifecycle: deleting a terminal KILLS its tmux session for good — gated behind
  // a themed confirm (the trash control / shift-click on the ×).
  const deleteTerminal = useWorkspace((s) => s.deleteTerminal);
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
  // Hold Shift -> the close (×) control morphs into a delete (kill-session)
  // control across every tile, with a destructive look + label. Shared tracker
  // (one window listener) so a wall of tiles doesn't each bind keydown/keyup.
  const shiftHeld = useShiftHeld();

  // Per-tile panel state (the Terminal / Files / Preview / Dev workbench). Kept
  // in usePanels — NOT workspace.ts — so this presentational state doesn't
  // contend with the workspace store. The active tab decides whether the body
  // shows the pooled terminal (terminal) or an in-tile surface (files/preview/
  // dev); fullscreen blows this one tile up to fill the window.
  const activeTab = usePanels((s) => s.tab[terminalId] ?? "terminal");
  const setTab = usePanels((s) => s.setTab);
  const toggleFullscreen = usePanels((s) => s.toggleFullscreen);
  const isFullscreen = usePanels((s) => s.fullscreenId === terminalId);

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

  // Whether the "delete session" confirm is up for this tile. The destructive
  // kill only runs once the user confirms (button / Enter); cancel/Esc/backdrop
  // dismiss it. Detach (the plain ×) needs no confirm — tmux survives.
  const [confirmDelete, setConfirmDelete] = useState(false);
  // Right-click context menu position (null = closed). Right-clicking the header
  // opens Close / Delete actions at the pointer.
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number } | null>(null);

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
          >
            {cwd}
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

        {/* ONE lifecycle control that MORPHS with Shift (per the user's model):
            - default  →  "×"  : CLOSE the terminal (detach; tmux session lives on)
            - Shift    →  trash: DELETE the terminal from the session (kills tmux),
                          gated behind a confirm. Holding Shift recolors it to the
                          error tone + swaps the glyph + tooltip so it's obvious the
                          next click destroys. The actual action keys off the real
                          event modifier (e.shiftKey), so it's correct even if the
                          tracked state lags by a frame. */}
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            if (e.shiftKey) setConfirmDelete(true);
            else onClose();
          }}
          className="shrink-0 rounded px-1 leading-none hover:bg-neutral-800"
          style={{ color: shiftHeld ? "var(--th-dot-error)" : "var(--th-fg-muted)" }}
          title={
            shiftHeld
              ? "Delete terminal from session (kills tmux — asks first)"
              : "Close terminal (keeps the session alive; hold Shift to delete)"
          }
          aria-label={shiftHeld ? "Delete terminal from session" : "Close terminal"}
        >
          {shiftHeld ? (
            // Inline trash glyph; inherits currentColor so it follows the theme.
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
              <path d="M2.5 4h11M6 4V2.5h4V4M5 4l.5 9.5h5L11 4M6.5 6.5v5M9.5 6.5v5" />
            </svg>
          ) : (
            "×"
          )}
        </button>
      </div>

      {/* Destructive confirm for deleting (killing) this terminal's session. */}
      <ConfirmDialog
        open={confirmDelete}
        title="Delete session?"
        body={
          <>
            This permanently kills the tmux session{" "}
            <span className="font-mono" style={{ color: "var(--th-fg)" }}>
              {terminalId}
            </span>{" "}
            and everything running in it. This can't be undone. To just close the
            tile and keep the session running, use Detach (×) instead.
          </>
        }
        confirmLabel="Delete session"
        onConfirm={() => {
          setConfirmDelete(false);
          deleteTerminal(terminalId);
        }}
        onCancel={() => setConfirmDelete(false)}
      />

      {/* Right-click context menu: Close (detach, session lives on in the
          background) or Delete (kill the session, behind the confirm). */}
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
              label="Close terminal"
              hint="Detach — keeps the session running in the background"
              onClick={() => {
                setCtxMenu(null);
                onClose();
              }}
            />
            <CtxItem
              label="Delete session"
              hint="Kills the tmux session — can't be undone"
              danger
              onClick={() => {
                setCtxMenu(null);
                setConfirmDelete(true);
              }}
            />
          </div>
        </>
      )}

      {/* Body. On the Terminal tab this is the EMPTY placeholder box marking
          where the pooled xterm should sit: the actual xterm is rendered ONCE in
          the persistent pool overlay (TerminalPool.tsx) and positioned over this
          box, so moving/resizing the tile never remounts/reattaches it (#20).
          On the other tabs we render the chosen surface (Files/Preview/Dev) in
          the body INSTEAD, and the pool parks this terminal (hidden offscreen)
          because it only SHOWs a terminal whose tile is on the Terminal tab —
          see TerminalPool.sync(). The placeholder must stay MOUNTED on the
          terminal tab so the pool has a rect to position over; we don't render it
          at all on the other tabs (the pool keys "show" off the active tab, not
          off the placeholder's presence). */}
      {isFullscreen && !slotActive ? (
        // This is the COVERED grid copy of the fullscreen tile (the fullscreen
        // layer renders the visible copy). It's entirely hidden behind the
        // fullscreen layer, so render an empty body: don't register a placeholder
        // (slotActive is already false) and don't duplicate the Files/Preview/Dev
        // surface (which would double-index/double-iframe needlessly). The Tile
        // chrome above still renders so its drop target (data-tile-id) stays in
        // the grid for drag resolution.
        <div className="min-h-0 flex-1 overflow-hidden" />
      ) : activeTab === "terminal" ? (
        // Attach the pool placeholder ref ONLY when this instance owns the slot
        // (slotActive). When a fullscreen tile is also rendered in its hidden
        // grid cell, that grid copy passes slotActive={false} so it doesn't
        // re-register and steal the placeholder from the fullscreen copy.
        <div
          ref={slotActive ? slotRef : undefined}
          className="min-h-0 flex-1 overflow-hidden"
        />
      ) : (
        <div className="min-h-0 flex-1 overflow-hidden">
          <TilePanel terminalId={terminalId} cwd={cwd} tab={activeTab} />
        </div>
      )}
    </div>
  );
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
