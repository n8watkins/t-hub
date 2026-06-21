// The sidebar's "Workspaces" list — the ONLY navigation surface now.
//
// In the product model a WORKSPACE is a top tab; a terminal lives inside one.
// (The old separate "Projects" section is gone — workspaces are the unit.) This
// shows EVERY workspace as a collapsible row over its terminals, the single place
// to switch to any workspace / any terminal, rename a workspace, and close a
// terminal.
//
// Behavior:
//   - Click a workspace row     -> switch to it (setActiveTab). Active workspace
//     gets a SUBTLE accent tint + thin accent left bar (not a bright fill).
//   - Double-click the name     -> rename the workspace inline (renameTab).
//   - Chevron                   -> expand/collapse its terminals (active expanded
//     by default; local UI state, not persisted).
//   - Click a terminal          -> switch to its workspace AND focus it.
//   - X on a terminal           -> kill it (deleteTerminal) — close what you're
//     working on right from here.
//
// Reads the workspace store directly (no props), so App needs no extra wiring.
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useWorkspace, deriveLabel } from "../store/workspace";
import type { WorkspaceTab } from "../store/workspace";
import { useTheme, WORKSPACE_COLOR_PALETTE } from "../store/theme";
import { useSupervision, tmuxSessionMidTurn } from "../store/supervision";
import { useActivity } from "../store/activity";
import { sessionNameForTerminal } from "../store/sessionContext";
import { clientForTerminal } from "../store/clientType";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";
import type { TerminalId, TerminalInfo, TerminalState } from "../ipc/types";
import { startPointerDrag, type PointerDragCanceller } from "../lib/pointerDrag";
import { resolveDropTarget } from "../lib/dropTarget";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { ChevronIcon, CountBadge } from "./SidebarChrome";

/** Lifecycle-dot color per terminal state — the SAME `--th-dot-*` palette Tile
 *  uses, so a terminal reads identically wherever it appears. */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

export function WorkspacesList() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const terminals = useWorkspace((s) => s.terminals);
  const labels = useWorkspace((s) => s.labels);
  const focusedId = useWorkspace((s) => s.focusedId);
  const focusedRegion = useWorkspace((s) => s.focusedRegion);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const setFocus = useWorkspace((s) => s.setFocus);
  const setFocusRegion = useWorkspace((s) => s.setFocusRegion);
  const renameTab = useWorkspace((s) => s.renameTab);
  const deleteTerminal = useWorkspace((s) => s.deleteTerminal);
  // Moving a terminal into another workspace by dragging its row (D2) reuses the
  // same store action the tile-header drag uses. setDraggingTile marks the
  // terminal as "being dragged" app-wide so its CANVAS tile dims too (the same
  // affordance a tile-header drag gives), keeping the two entry points consistent.
  const moveTileToTab = useWorkspace((s) => s.moveTileToTab);
  const setDraggingTile = useWorkspace((s) => s.setDraggingTile);
  // Reorder WHOLE workspaces in the sidebar by dragging a workspace row header.
  // moveTab(id, targetId) moves `id` into `targetId`'s slot and persists via the
  // same snapshot mechanism every other tab edit uses.
  const moveTab = useWorkspace((s) => s.moveTab);
  // Per-workspace color identity (feat/workspace-colors): the dot + a quick color
  // picker live on each workspace row. Read from the theme store (mirrors the
  // per-terminal override slots).
  const workspaceColors = useTheme((s) => s.workspaceColors);
  const setWorkspaceColor = useTheme((s) => s.setWorkspaceColor);
  const clearWorkspaceColor = useTheme((s) => s.clearWorkspaceColor);
  // Per-terminal color identity (the SAME slot the tile ⋯ menu writes): setting a
  // terminal's color here recolors its canvas tile too, and wins over the
  // workspace color on that terminal's sidebar row.
  const termFocusRing = useTheme((s) => s.termFocusRing);
  const setTermFocusRing = useTheme((s) => s.setTermFocusRing);
  const clearTermFocusRing = useTheme((s) => s.clearTermFocusRing);
  // The sidebar region being focused (Ctrl+B) highlights the ACTIVE workspace row
  // so keyboard nav reads clearly.
  const sidebarFocused = focusedRegion === "sidebar";

  // Local expand/collapse, keyed by tab id. Undefined = default (open iff active).
  const [openMap, setOpenMap] = useState<Record<string, boolean>>({});
  const isOpen = (tab: WorkspaceTab) => openMap[tab.id] ?? tab.id === activeTabId;
  const toggleOpen = (tab: WorkspaceTab) =>
    setOpenMap((m) => ({ ...m, [tab.id]: !isOpen(tab) }));

  // --- D2: drag a terminal row into another workspace --------------------
  // The terminal being dragged (its row dims) and the workspace row currently
  // under the pointer (it lights up as a drop target). Pointer-based, like every
  // other T-Hub drag — HTML5 DnD dies over xterm (see lib/pointerDrag.ts).
  const [dragTerminalId, setDragTerminalId] = useState<TerminalId | null>(null);
  const [dropTabId, setDropTabId] = useState<string | null>(null);
  // --- Reorder workspaces -------------------------------------------------
  // Dragging a WORKSPACE ROW reorders the workspace list. `dragWsId` is the row
  // being moved (it dims); `wsDropTabId` is the workspace row currently under the
  // pointer (it lights up as the insertion target). Mirrors the terminal-row drag
  // above, resolving the target via the same `data-th-ws-row` anchor.
  const [dragWsId, setDragWsId] = useState<string | null>(null);
  const [wsDropTabId, setWsDropTabId] = useState<string | null>(null);
  const wsDragCancelRef = useRef<PointerDragCanceller | null>(null);
  useEffect(() => {
    return () => {
      wsDragCancelRef.current?.();
      wsDragCancelRef.current = null;
    };
  }, []);
  // The workspace the dragged terminal currently lives in — so we never flag its
  // OWN workspace as a (no-op) drop target. Captured ONCE when the drag begins
  // (it can't change mid-gesture), not re-derived on every move-driven re-render.
  const sourceTabRef = useRef<string | null>(null);
  // Canceller for an in-flight terminal-row drag (#3): set while a drag is live,
  // nulled when it ends, and invoked from the unmount cleanup below so the list
  // unmounting mid-drag can't leak window listeners or a stuck `data-th-dragging`
  // body flag that leaves every terminal pointer-inert.
  const dragCancelRef = useRef<PointerDragCanceller | null>(null);
  useEffect(() => {
    return () => {
      dragCancelRef.current?.();
      dragCancelRef.current = null;
    };
  }, []);

  // Resolve which workspace row sits under a viewport point. Terminals are made
  // pointer-inert during the drag (data-th-dragging), so elementFromPoint lands
  // on the sidebar chrome; we walk up to the owning workspace row.
  const workspaceRowAt = (x: number, y: number): string | null =>
    resolveDropTarget(x, y, ["[data-th-ws-row]"])?.value ?? null;

  // Shared pointer-drag spine for BOTH sidebar drags (a terminal row into a
  // workspace, and a workspace row reorder). It owns the parts both gestures had
  // copied verbatim: the left-button guard, the floating ghost lifecycle, the
  // drag controller (which owns the `data-th-dragging` body flag), the canceller
  // stash (so the unmount cleanup above can abort a live drag, #3), the
  // `data-th-ws-row` drop-target resolution on move/end, and the `onSettled`
  // click-suppression. Each caller supplies only what differs: the ghost label,
  // its drag/drop state setters, and the commit. `onSettled(committed)` lets the
  // source neutralize the synthetic click that trails a committed drag.
  const startSidebarDrag = (
    e: ReactPointerEvent,
    cancelRef: React.MutableRefObject<PointerDragCanceller | null>,
    opts: {
      threshold?: number;
      ghostTitle: string;
      onBegin?: () => void;
      setDropTarget: (target: string | null) => void;
      onEnd?: () => void;
      commit: (target: string) => void;
      onSettled?: (committed: boolean) => void;
    },
  ) => {
    if (e.button !== 0) return; // primary (left) button only
    let ghost: DragGhost | null = null;
    cancelRef.current = startPointerDrag(e.clientX, e.clientY, {
      manageBodyDragFlag: true,
      threshold: opts.threshold,
      onBegin: () => {
        opts.onBegin?.();
        ghost = createDragGhost({ title: opts.ghostTitle, width: 200 });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        opts.setDropTarget(workspaceRowAt(x, y));
      },
      onEnd: (x, y, committed) => {
        const target = committed ? workspaceRowAt(x, y) : null;
        ghost?.destroy();
        ghost = null;
        cancelRef.current = null;
        opts.onEnd?.();
        opts.setDropTarget(null);
        // Tell the source whether a drag happened (suppress its trailing click)
        // BEFORE the browser dispatches that click, then commit.
        opts.onSettled?.(committed);
        if (target) opts.commit(target);
      },
    });
  };

  const startTerminalDrag = (
    id: TerminalId,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) =>
    startSidebarDrag(e, dragCancelRef, {
      ghostTitle: deriveLabel({
        id,
        label: labels[id],
        title: terminals[id]?.title,
        cwd: terminals[id]?.cwd,
      }),
      onBegin: () => {
        // The source workspace is fixed for the whole gesture — resolve it once.
        sourceTabRef.current = tabs.find((t) => t.order.includes(id))?.id ?? null;
        setDragTerminalId(id);
        // Mark the terminal as dragged app-wide so its canvas tile dims too.
        setDraggingTile(id);
      },
      setDropTarget: setDropTabId,
      onEnd: () => {
        setDraggingTile(null);
        setDragTerminalId(null);
        sourceTabRef.current = null;
      },
      // moveTileToTab no-ops on same/unknown tab, so an in-place drop is safe.
      commit: (target) => moveTileToTab(id, target),
      onSettled,
    });

  // Reorder a WHOLE workspace row. Larger threshold (10px vs default 4) because
  // the workspace name is BOTH the drag handle and the double-click-to-rename
  // target — a quick double-click that drifts must NOT begin a reorder.
  const startWorkspaceDrag = (
    id: string,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) =>
    startSidebarDrag(e, wsDragCancelRef, {
      threshold: 10,
      ghostTitle: tabs.find((t) => t.id === id)?.name ?? "Workspace",
      onBegin: () => setDragWsId(id),
      setDropTarget: setWsDropTabId,
      onEnd: () => setDragWsId(null),
      // moveTab no-ops on same/unknown id, so an in-place drop is safe.
      commit: (target) => {
        if (target !== id) moveTab(id, target);
      },
      onSettled,
    });

  if (tabs.length === 0) {
    return (
      <div className="px-2 py-1 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        No workspaces yet.
      </div>
    );
  }

  return (
    <ul className="flex flex-col gap-0.5 px-2 py-1">
      {tabs.map((tab) => (
        <WorkspaceRow
          key={tab.id}
          tab={tab}
          active={tab.id === activeTabId}
          open={isOpen(tab)}
          terminals={terminals}
          labels={labels}
          focusedId={focusedId}
          color={workspaceColors[tab.id]}
          termFocusRing={termFocusRing}
          // The active row is the keyboard-nav focus target while the sidebar
          // region is focused (Ctrl+B), so highlight it then.
          navFocused={sidebarFocused && tab.id === activeTabId}
          // D2 drag: light this row up as a drop target only when a terminal from
          // a DIFFERENT workspace is hovering it; its terminals are drag sources.
          isDropTarget={
            dragTerminalId != null &&
            dropTabId === tab.id &&
            tab.id !== sourceTabRef.current
          }
          draggingTerminalId={dragTerminalId}
          onTerminalDragStart={startTerminalDrag}
          // Reorder drag: this row is a drag SOURCE (header) and a drop target for
          // another workspace row being dragged over it.
          isWsDragging={dragWsId === tab.id}
          isWsDropTarget={
            dragWsId != null && wsDropTabId === tab.id && dragWsId !== tab.id
          }
          onWorkspaceDragStart={startWorkspaceDrag}
          onToggle={() => toggleOpen(tab)}
          onActivate={() => {
            // Activating from the sidebar keeps nav focus IN the sidebar so a
            // following Ctrl+Tab keeps cycling workspaces.
            setActiveTab(tab.id);
            setFocusRegion("sidebar");
          }}
          onRename={(name) => renameTab(tab.id, name)}
          onSetColor={(c) => setWorkspaceColor(tab.id, c)}
          onClearColor={() => clearWorkspaceColor(tab.id)}
          // Per-terminal color writes the SAME slot the tile ⋯ menu uses, so a
          // color set here recolors that terminal's canvas tile too.
          onSetTerminalColor={(id, c) => setTermFocusRing(id, c)}
          onClearTerminalColor={(id) => clearTermFocusRing(id)}
          onSelectTerminal={(id) => {
            // Clicking a terminal jumps to the canvas (setFocus moves nav focus to
            // the terminal region).
            setActiveTab(tab.id);
            setFocus(id);
          }}
          onCloseTerminal={(id) => deleteTerminal(id)}
        />
      ))}
    </ul>
  );
}

function WorkspaceRow({
  tab,
  active,
  open,
  terminals,
  labels,
  focusedId,
  color,
  termFocusRing,
  navFocused,
  isDropTarget,
  draggingTerminalId,
  onTerminalDragStart,
  isWsDragging,
  isWsDropTarget,
  onWorkspaceDragStart,
  onToggle,
  onActivate,
  onRename,
  onSetColor,
  onClearColor,
  onSetTerminalColor,
  onClearTerminalColor,
  onSelectTerminal,
  onCloseTerminal,
}: {
  tab: WorkspaceTab;
  active: boolean;
  open: boolean;
  terminals: Record<TerminalId, TerminalInfo>;
  labels: Record<TerminalId, string>;
  focusedId: TerminalId | null;
  /** This workspace's assigned color (undefined => follow the default accent). */
  color?: string;
  /** Per-terminal override colors (terminalId → color) — a terminal's OWN color
   *  beats the workspace color on its sidebar row. */
  termFocusRing: Record<string, string>;
  /** True when this row is the sidebar's keyboard-nav focus target (Ctrl+B). */
  navFocused: boolean;
  /** True when a terminal from another workspace is being dragged over this row
   *  (D2) — drives the drop affordance. */
  isDropTarget: boolean;
  /** The terminal currently being dragged (its row dims), or null. */
  draggingTerminalId: TerminalId | null;
  /** Begin dragging one of this row's terminals into another workspace.
   *  `onSettled(committed)` fires on release so the row can suppress the click
   *  that may trail a committed drag. */
  onTerminalDragStart: (
    id: TerminalId,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) => void;
  /** True while THIS workspace row is the one being dragged to reorder (it dims). */
  isWsDragging: boolean;
  /** True when ANOTHER workspace row is being dragged over this one (drop target). */
  isWsDropTarget: boolean;
  /** Begin dragging this whole workspace row to reorder it. `onSettled(committed)`
   *  fires on release so the row can suppress the trailing click. */
  onWorkspaceDragStart: (
    id: string,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) => void;
  onToggle: () => void;
  onActivate: () => void;
  onRename: (name: string) => void;
  onSetColor: (color: string) => void;
  onClearColor: () => void;
  /** Set/clear a single terminal's color (writes the per-terminal theme slot). */
  onSetTerminalColor: (id: TerminalId, color: string) => void;
  onClearTerminalColor: (id: TerminalId) => void;
  onSelectTerminal: (id: TerminalId) => void;
  onCloseTerminal: (id: TerminalId) => void;
}) {
  const count = tab.order.length;
  // Inline rename state: double-click the name to edit; Enter/blur commits,
  // Esc cancels. Seeded from the current name each time editing starts.
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(tab.name);
  const inputRef = useRef<HTMLInputElement>(null);
  // Color-picker popover open state (the dot). Anchored under the dot button.
  const [colorMenu, setColorMenu] = useState(false);
  // Which terminal row's color picker is open (its id), or null. Only one at a time.
  const [termColorMenuId, setTermColorMenuId] = useState<TerminalId | null>(null);
  const activateRef = useRef<HTMLButtonElement>(null);
  // Swallows the synthetic click that trails a committed workspace-row drag, so a
  // reorder gesture never also activates this workspace. Cleared on each press.
  const wsSuppressClickRef = useRef(false);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  // The active-workspace accent: the workspace color if set, else the global
  // theme accent. Drives the active row tint + left bar so the sidebar reflects
  // the workspace identity.
  const accent = color ?? "var(--th-accent)";

  const startEdit = () => {
    setDraft(tab.name);
    setEditing(true);
  };
  const commit = () => {
    const name = draft.trim();
    if (name && name !== tab.name) onRename(name);
    setEditing(false);
  };

  return (
    // data-th-ws-row marks this whole workspace block as a D2 drop target: a
    // dragged terminal resolves to it via elementFromPoint + closest.
    <li data-th-ws-row={tab.id}>
      <div
        className="flex w-full items-center gap-1 rounded-lg pr-1 transition-colors hover:bg-neutral-800/40"
        style={{
          color: "var(--th-fg)",
          // Dim this row while it is the workspace being dragged to reorder.
          opacity: isWsDragging ? 0.4 : undefined,
          ...(active
            ? {
                backgroundColor: `color-mix(in srgb, ${accent} 16%, transparent)`,
                boxShadow: `inset 2px 0 0 0 ${accent}`,
              }
            : {}),
          // A clear focus ring when this row is the sidebar's keyboard target.
          ...(navFocused
            ? { outline: `1px solid ${accent}`, outlineOffset: "-1px" }
            : {}),
          // A drop terminal is hovering this workspace: a crisp accent ring +
          // tint so the target reads clearly (wins over active/nav styling).
          ...(isDropTarget
            ? {
                backgroundColor: `color-mix(in srgb, ${accent} 24%, transparent)`,
                outline: `2px dashed ${accent}`,
                outlineOffset: "-2px",
              }
            : {}),
          // Another workspace row is being dragged over this one (reorder): a
          // dashed insertion ring so the drop slot reads clearly.
          ...(isWsDropTarget
            ? {
                backgroundColor: `color-mix(in srgb, ${accent} 20%, transparent)`,
                outline: `2px dashed ${accent}`,
                outlineOffset: "-2px",
              }
            : {}),
        }}
      >
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={open ? "Collapse workspace" : "Expand workspace"}
          title={open ? "Collapse" : "Expand"}
          className="flex h-7 w-5 shrink-0 items-center justify-center opacity-70 hover:opacity-100"
        >
          <ChevronIcon open={open} />
        </button>

        {/* Workspace color dot — click to open a small palette + custom picker.
            The dot shows the assigned color (or the muted default when unset). */}
        <div className="relative shrink-0">
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setColorMenu((v) => !v);
            }}
            className="flex h-5 w-5 items-center justify-center rounded hover:bg-neutral-700/40"
            title="Workspace color"
            aria-label="Set workspace color"
            aria-haspopup="menu"
            aria-expanded={colorMenu}
          >
            <span
              className="h-2.5 w-2.5 rounded-full"
              style={{
                backgroundColor: color ?? "var(--th-fg-muted)",
                boxShadow: color ? `0 0 5px -1px ${color}` : undefined,
              }}
            />
          </button>
          {colorMenu && (
            <ColorPicker
              current={color}
              onPick={(c) => onSetColor(c)}
              onClear={() => {
                onClearColor();
                setColorMenu(false);
              }}
              onClose={() => setColorMenu(false)}
            />
          )}
        </div>

        {editing ? (
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
              else if (e.key === "Escape") setEditing(false);
            }}
            spellCheck={false}
            className="min-w-0 flex-1 rounded bg-transparent px-1 py-1 text-sm outline-none"
            style={{
              color: "var(--th-fg)",
              border: `1px solid ${accent}`,
            }}
          />
        ) : (
          <button
            ref={activateRef}
            type="button"
            // The active row is the sidebar's keyboard-nav focus target (Ctrl+B
            // focuses this; Ctrl+Tab then cycles workspaces).
            data-th-sidebar-focus={active ? "" : undefined}
            onClick={() => {
              // Swallow exactly the click that trails a committed reorder drag; a
              // plain click (or keyboard activation) activates as normal.
              if (wsSuppressClickRef.current) {
                wsSuppressClickRef.current = false;
                return;
              }
              onActivate();
            }}
            // Press-and-drag the name to reorder this workspace among the others.
            // A plain click (no movement past the threshold) still activates it.
            // touch-none/select-none keep touch/pen + text selection from stealing
            // the gesture, matching the terminal-row drag handle.
            onPointerDown={(e) => {
              wsSuppressClickRef.current = false;
              onWorkspaceDragStart(tab.id, e, (committed) => {
                wsSuppressClickRef.current = committed;
              });
            }}
            onDoubleClick={startEdit}
            aria-current={active ? "true" : undefined}
            className="flex min-w-0 flex-1 cursor-pointer touch-none select-none items-center gap-2 py-1.5 pr-1 text-left text-sm outline-none"
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"} · double-click to rename · drag to reorder`}
          >
            <span className="min-w-0 flex-1 truncate font-medium">{tab.name}</span>
            <CountBadge n={count} />
          </button>
        )}
      </div>

      {/* Smooth expand/collapse via a 0fr↔1fr grid row (rows stay mounted). */}
      <div
        className="grid"
        style={{
          gridTemplateRows: open ? "1fr" : "0fr",
          transition: "grid-template-rows 200ms ease",
        }}
      >
        <div style={{ overflow: "hidden", minHeight: 0 }}>
          {count === 0 ? (
            <div
              className="py-1 pl-7 pr-2 text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Nothing open here yet.
            </div>
          ) : (
            <ul className="flex flex-col gap-0.5 py-0.5 pl-5">
              {tab.order.map((id) => (
                <TerminalRow
                  key={id}
                  id={id}
                  info={terminals[id]}
                  userLabel={labels[id]}
                  active={id === focusedId}
                  dragging={draggingTerminalId === id}
                  // The row's identity color: this terminal's OWN override wins;
                  // otherwise the workspace color cascades down. Undefined => the
                  // row follows the default (no tint).
                  rowColor={termFocusRing[id] ?? color}
                  ownColor={termFocusRing[id]}
                  colorMenuOpen={termColorMenuId === id}
                  onToggleColorMenu={() =>
                    setTermColorMenuId((cur) => (cur === id ? null : id))
                  }
                  onCloseColorMenu={() => setTermColorMenuId(null)}
                  onSetColor={(c) => onSetTerminalColor(id, c)}
                  onClearColor={() => {
                    onClearTerminalColor(id);
                    setTermColorMenuId(null);
                  }}
                  onDragStart={onTerminalDragStart}
                  onClick={() => onSelectTerminal(id)}
                  onClose={() => onCloseTerminal(id)}
                />
              ))}
            </ul>
          )}
        </div>
      </div>
    </li>
  );
}

/** Worktree-aware folder detail for a terminal row, mirroring the Recent list
 *  (RecentList.tsx `cwdBasename`/`cwdWorktree`). The agent ICON already conveys
 *  claude/codex, so the row shows WHERE the work lives — the folder, plus a
 *  `· <worktree>` hint (a `wt-<branch>` segment, else the parent project folder) —
 *  instead of repeating the command word. Returns "" for an empty cwd. */
function folderDetail(cwd: string): string {
  const parts = cwd
    .replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean);
  if (parts.length === 0) return "";
  const name = parts[parts.length - 1];
  let worktree = "";
  for (let i = parts.length - 1; i >= 0; i -= 1) {
    if (/^wt-/.test(parts[i])) {
      worktree = parts[i].replace(/^wt-/, "");
      break;
    }
  }
  if (!worktree && parts.length >= 2) worktree = parts[parts.length - 2];
  return worktree && worktree !== name ? `${name} · ${worktree}` : name;
}

/** One nested terminal row: the agent icon + a themed lifecycle dot (pulses while
 *  the bound session is mid-turn) + the folder/worktree detail (NOT the command
 *  word — the icon already says claude/codex — and NOT the opaque session id), and
 *  an X to close it. The focused tile of the active workspace gets the accent tint. */
function TerminalRow({
  id,
  info,
  userLabel,
  active,
  dragging,
  rowColor,
  ownColor,
  colorMenuOpen,
  onToggleColorMenu,
  onCloseColorMenu,
  onSetColor,
  onClearColor,
  onDragStart,
  onClick,
  onClose,
}: {
  id: TerminalId;
  info?: TerminalInfo;
  userLabel?: string;
  active: boolean;
  /** True while THIS terminal is the one being dragged (the row dims). */
  dragging: boolean;
  /** The row's effective identity color: this terminal's own override if set,
   *  else the owning workspace's color. Undefined => no tint (default). */
  rowColor?: string;
  /** This terminal's OWN override color (drives the swatch fill + the clear
   *  button's enabled state), undefined when it only inherits the workspace. */
  ownColor?: string;
  /** Whether this row's color picker popover is open. */
  colorMenuOpen: boolean;
  /** Toggle this row's color picker open/closed. */
  onToggleColorMenu: () => void;
  /** Close this row's color picker. */
  onCloseColorMenu: () => void;
  /** Set this terminal's color (writes the per-terminal theme slot). */
  onSetColor: (color: string) => void;
  /** Clear this terminal's color (follow the workspace / default again). */
  onClearColor: () => void;
  /** Begin a pointer-drag of this terminal into another workspace (D2). */
  onDragStart: (
    id: TerminalId,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) => void;
  onClick: () => void;
  onClose: () => void;
}) {
  const state: TerminalState = info?.state ?? "starting";
  // ACTIVITY: pulse this row's lifecycle dot while the bound Claude session is
  // mid-turn (working / waiting on subagents / needs*). Cheap CSS (animate-pulse),
  // and nothing animates when the session is idle. Keyed by `th_<id>`, the same
  // session name the tile uses (see store/sessionContext).
  const working = useSupervision((s) =>
    tmuxSessionMidTurn(s, sessionNameForTerminal(id)),
  );
  // RUNNING animation (#11): also pulse while the terminal is actively producing
  // output (store/activity) — the cross-agent proxy for Codex (no mid-turn hooks)
  // and shells running a command. Claude keeps its precise supervision pulse above.
  const outputActive = useActivity((s) => !!s.active[id]);
  const pulsing = working || outputActive;
  // Which agent runs here (claude/codex/shell) — drives the leading icon so the
  // sidebar row reads as the AGENT, not just a generic dot.
  const client = clientForTerminal(id);
  const label = deriveLabel({
    id,
    label: userLabel,
    title: info?.title,
    cwd: info?.cwd,
  });
  const cwd = info?.cwd ?? "";
  // The user's cosmetic "work name" for this project (keyed by cwd) — shown as the
  // primary line when set, with the derived command·dir label as a muted subtitle.
  const workName = useTheme((s) => (cwd ? s.workNames[cwd] : undefined));
  // #10: the agent ICON conveys claude/codex, so the row text shows WHERE the work
  // lives (folder + worktree, like the Recent list) rather than the command word.
  // Falls back to the derived label only when there's no cwd yet (fresh spawn).
  const detail = folderDetail(cwd) || label;

  // A committed drag (pointerup after crossing the move threshold) can be
  // followed by a synthetic click on this button; this ref lets us swallow that
  // one click so a drag never also selects. Cleared at the start of every press,
  // so it can't leak into a later, genuine click.
  const suppressClickRef = useRef(false);
  return (
    <li
      className="group relative flex items-center gap-2 rounded-lg pr-1 transition-colors hover:bg-neutral-800/40"
      style={{
        color: "var(--th-fg)",
        // Dim the source row while it's being dragged into another workspace.
        opacity: dragging ? 0.4 : undefined,
        // COLOR CASCADE: a subtle identity tint + a thin left accent bar drawn
        // from the row's effective color (own override, else the workspace color).
        // The same color-mix subtlety the canvas tiles use. The focused/active
        // treatment below LAYERS ON TOP (a stronger tint + bar) so it still wins.
        ...(rowColor
          ? {
              backgroundColor: `color-mix(in srgb, ${rowColor} 10%, transparent)`,
              boxShadow: `inset 2px 0 0 0 color-mix(in srgb, ${rowColor} 70%, transparent)`,
            }
          : {}),
        ...(active
          ? {
              backgroundColor: rowColor
                ? `color-mix(in srgb, ${rowColor} 20%, transparent)`
                : "color-mix(in srgb, var(--th-accent) 16%, transparent)",
              boxShadow: `inset 2px 0 0 0 ${rowColor ?? "var(--th-accent)"}`,
            }
          : {}),
      }}
    >
      <button
        type="button"
        onClick={() => {
          // Swallow exactly the click that trails a committed drag; a plain
          // click (or keyboard activation) selects as normal.
          if (suppressClickRef.current) {
            suppressClickRef.current = false;
            return;
          }
          onClick();
        }}
        // Press-and-drag this row to move the terminal into another workspace
        // (D2). A plain click (no movement past the threshold) still selects it.
        // touch-none/select-none keep touch/pen + text selection from stealing
        // the gesture, matching the tile-header drag handle.
        onPointerDown={(e) => {
          suppressClickRef.current = false;
          onDragStart(id, e, (committed) => {
            suppressClickRef.current = committed;
          });
        }}
        aria-current={active ? "true" : undefined}
        className="flex min-w-0 flex-1 cursor-pointer touch-none select-none items-center gap-2 py-1.5 pl-2.5 text-left text-sm"
        title={cwd ? `${label} — ${cwd} (${state})` : `${label} — ${state}`}
      >
        {/* Agent icon (claude/codex) so the row reads as the agent at a glance;
            plain shells show no icon (just the dot). */}
        {client === "claude" ? (
          <ClaudeIcon
            size={14}
            className="shrink-0"
            style={{ color: "#D97757" }}
            title="Claude"
          />
        ) : client === "codex" ? (
          <CodexIcon size={14} className="shrink-0" title="Codex" />
        ) : null}
        <span
          // ACTIVITY: pulse while the agent is working — Claude mid-turn
          // (supervision) OR any live output (Codex / a shell running a command);
          // static when idle.
          className={`h-2 w-2 shrink-0 rounded-full${pulsing ? " animate-pulse" : ""}`}
          style={{
            backgroundColor: DOT_VAR[state],
            // A soft glow while active makes the pulse read even on a tiny dot.
            boxShadow: pulsing ? `0 0 5px 0 ${DOT_VAR[state]}` : undefined,
          }}
          aria-hidden
          title={pulsing ? "Working…" : undefined}
        />
        <span className="min-w-0 flex-1">
          <span className="block truncate">{workName ?? detail}</span>
          {workName && (
            <span
              className="block truncate text-[11px]"
              style={{ color: "var(--th-fg-muted)" }}
            >
              {detail}
            </span>
          )}
        </span>
      </button>
      {/* Per-terminal color circle — click to open a small palette + custom
          picker. Writes THIS terminal's color (the same per-terminal theme slot
          the tile ⋯ menu uses), so a change here recolors its canvas tile too.
          Reveals on row hover (or stays visible while its menu is open / a color
          is set) so it doesn't clutter the list. */}
      <div className="relative shrink-0">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onToggleColorMenu();
          }}
          // Hover-only: hidden until the row is hovered (group-hover), and kept
          // visible only while its own picker is open. The row already shows the
          // assigned color as its tint/accent bar, so the swatch button itself
          // doesn't need to persist when a color is set.
          className={`flex h-5 w-5 items-center justify-center rounded transition-opacity hover:bg-neutral-700/40 group-hover:opacity-100${
            colorMenuOpen ? " opacity-100" : " opacity-0"
          }`}
          title="Terminal color"
          aria-label="Set terminal color"
          aria-haspopup="menu"
          aria-expanded={colorMenuOpen}
        >
          <span
            className="h-2.5 w-2.5 rounded-full"
            style={{
              backgroundColor: ownColor ?? "var(--th-fg-muted)",
              boxShadow: ownColor ? `0 0 5px -1px ${ownColor}` : undefined,
              border: ownColor ? undefined : "1px solid var(--th-border)",
            }}
          />
        </button>
        {colorMenuOpen && (
          <ColorPicker
            title="Terminal color"
            current={ownColor}
            onPick={onSetColor}
            onClear={onClearColor}
            onClose={onCloseColorMenu}
          />
        )}
      </div>
      {/* Close (kill) this terminal — "close what we're working on from the
          workspace". Reveals on row hover so it doesn't clutter the list. */}
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
        className="shrink-0 rounded px-1 leading-none opacity-0 transition-opacity hover:bg-neutral-700/40 group-hover:opacity-100"
        style={{ color: "var(--th-fg-muted)" }}
        title="Close this terminal (kills its session)"
        aria-label="Close terminal"
      >
        ×
      </button>
    </li>
  );
}

/**
 * A small color-picker popover: a row of palette swatches, a custom
 * `<input type="color">`, and a "default" reset. Anchored under the swatch
 * button. A full-window backdrop dismisses it (mirrors the tile ⋯ color popover
 * pattern). Shared by the workspace dot and each terminal row's color circle —
 * the `title` distinguishes the two (defaults to "Workspace color").
 */
function ColorPicker({
  current,
  onPick,
  onClear,
  onClose,
  title = "Workspace color",
}: {
  current?: string;
  onPick: (color: string) => void;
  onClear: () => void;
  onClose: () => void;
  title?: string;
}) {
  return (
    <>
      <div
        className="fixed inset-0 z-40"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
      />
      <div
        className="absolute left-0 top-6 z-50 w-[176px] rounded-md border p-2 shadow-2xl"
        style={{
          // Solid surface so the picker never bleeds content through
          // (--th-header-bg carries alpha in some themes).
          backgroundColor: "var(--th-tile-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="mb-1.5 px-0.5 text-[10px] font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {title}
        </div>
        <div className="grid grid-cols-4 gap-1.5">
          {WORKSPACE_COLOR_PALETTE.map((c) => {
            const selected =
              !!current && current.toLowerCase() === c.toLowerCase();
            return (
              <button
                key={c}
                type="button"
                // A palette swatch is a discrete choice: set AND close. The
                // custom <input type="color"> below only calls onPick (no close)
                // so the native picker can stay open and be dragged live — onPick
                // must therefore be set-only at every call site.
                onClick={() => {
                  onPick(c);
                  onClose();
                }}
                className="flex h-6 w-full items-center justify-center rounded"
                style={{
                  backgroundColor: c,
                  outline: selected ? "2px solid var(--th-fg)" : undefined,
                  outlineOffset: "1px",
                }}
                title={c}
                aria-label={`Use ${c}`}
              />
            );
          })}
        </div>
        <div className="mt-2 flex items-center gap-2">
          <label
            className="flex flex-1 cursor-pointer items-center gap-1.5 text-xs"
            style={{ color: "var(--th-fg-muted)" }}
            title="Custom color"
          >
            <input
              type="color"
              value={current ?? "#38bdf8"}
              onChange={(e) => onPick(e.target.value)}
              className="h-6 w-9 shrink-0 cursor-pointer rounded bg-transparent p-0"
            />
            Custom
          </label>
          <button
            type="button"
            onClick={onClear}
            disabled={!current}
            className="rounded border px-2 py-1 text-xs hover:bg-neutral-800 disabled:opacity-40"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            title="Clear (follow the default accent)"
          >
            Default
          </button>
        </div>
      </div>
    </>
  );
}
