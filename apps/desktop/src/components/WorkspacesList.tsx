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
  // Per-workspace color identity (feat/workspace-colors): the dot + a quick color
  // picker live on each workspace row. Read from the theme store (mirrors the
  // per-terminal override slots).
  const workspaceColors = useTheme((s) => s.workspaceColors);
  const setWorkspaceColor = useTheme((s) => s.setWorkspaceColor);
  const clearWorkspaceColor = useTheme((s) => s.clearWorkspaceColor);
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
  // other TermHub drag — HTML5 DnD dies over xterm (see lib/pointerDrag.ts).
  const [dragTerminalId, setDragTerminalId] = useState<TerminalId | null>(null);
  const [dropTabId, setDropTabId] = useState<string | null>(null);
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

  // `onSettled(committed)` lets the source row neutralize the synthetic click
  // that can follow a committed drag, so a drag never doubles as a select.
  const startTerminalDrag = (
    id: TerminalId,
    e: ReactPointerEvent,
    onSettled?: (committed: boolean) => void,
  ) => {
    if (e.button !== 0) return; // primary (left) button only
    let ghost: DragGhost | null = null;
    // The drag controller now OWNS the `data-th-dragging` body flag
    // (manageBodyDragFlag), so this handler no longer sets/clears it by hand; the
    // stashed canceller lets the unmount cleanup above abort a live drag (#3).
    dragCancelRef.current = startPointerDrag(e.clientX, e.clientY, {
      manageBodyDragFlag: true,
      onBegin: () => {
        // The source workspace is fixed for the whole gesture — resolve it once.
        sourceTabRef.current =
          tabs.find((t) => t.order.includes(id))?.id ?? null;
        setDragTerminalId(id);
        // Mark the terminal as dragged app-wide so its canvas tile dims too.
        setDraggingTile(id);
        ghost = createDragGhost({
          title: deriveLabel({
            id,
            label: labels[id],
            title: terminals[id]?.title,
            cwd: terminals[id]?.cwd,
          }),
          width: 200,
        });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        setDropTabId(workspaceRowAt(x, y));
      },
      onEnd: (x, y, committed) => {
        const targetTab = committed ? workspaceRowAt(x, y) : null;
        ghost?.destroy();
        ghost = null;
        dragCancelRef.current = null;
        setDraggingTile(null);
        setDragTerminalId(null);
        setDropTabId(null);
        sourceTabRef.current = null;
        // Tell the source row whether a drag actually happened (suppress its
        // trailing click) BEFORE the browser dispatches that click.
        onSettled?.(committed);
        // moveTileToTab no-ops on same/unknown tab, so an in-place drop is safe.
        if (targetTab) moveTileToTab(id, targetTab);
      },
    });
  };

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
  navFocused,
  isDropTarget,
  draggingTerminalId,
  onTerminalDragStart,
  onToggle,
  onActivate,
  onRename,
  onSetColor,
  onClearColor,
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
  onToggle: () => void;
  onActivate: () => void;
  onRename: (name: string) => void;
  onSetColor: (color: string) => void;
  onClearColor: () => void;
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
  const activateRef = useRef<HTMLButtonElement>(null);

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
              onPick={(c) => {
                onSetColor(c);
                setColorMenu(false);
              }}
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
            onClick={onActivate}
            onDoubleClick={startEdit}
            aria-current={active ? "true" : undefined}
            className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 pr-1 text-left text-sm outline-none"
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"} · double-click to rename`}
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

/** One nested terminal row: a themed lifecycle dot + the friendly "what's
 *  running" label (e.g. "claude · tools" from deriveLabel — the command + dir,
 *  NOT the opaque session id, which isn't useful here), and an X to close it. The
 *  focused tile (of the active workspace) gets the subtle accent tint + left bar. */
function TerminalRow({
  id,
  info,
  userLabel,
  active,
  dragging,
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

  // A committed drag (pointerup after crossing the move threshold) can be
  // followed by a synthetic click on this button; this ref lets us swallow that
  // one click so a drag never also selects. Cleared at the start of every press,
  // so it can't leak into a later, genuine click.
  const suppressClickRef = useRef(false);
  return (
    <li
      className="group flex items-center gap-2 rounded-lg pr-1 transition-colors hover:bg-neutral-800/40"
      style={{
        color: "var(--th-fg)",
        // Dim the source row while it's being dragged into another workspace.
        opacity: dragging ? 0.4 : undefined,
        ...(active
          ? {
              backgroundColor:
                "color-mix(in srgb, var(--th-accent) 16%, transparent)",
              boxShadow: "inset 2px 0 0 0 var(--th-accent)",
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
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: DOT_VAR[state] }}
          aria-hidden
        />
        <span className="min-w-0 flex-1">
          <span className="block truncate">{workName ?? label}</span>
          {workName && (
            <span
              className="block truncate text-[11px]"
              style={{ color: "var(--th-fg-muted)" }}
            >
              {label}
            </span>
          )}
        </span>
      </button>
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
 * A small workspace-color picker popover: a row of palette swatches, a custom
 * `<input type="color">`, and a "default" reset. Anchored under the dot button. A
 * full-window backdrop dismisses it (mirrors the tile ⋯ color popover pattern).
 */
function ColorPicker({
  current,
  onPick,
  onClear,
  onClose,
}: {
  current?: string;
  onPick: (color: string) => void;
  onClear: () => void;
  onClose: () => void;
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
          backgroundColor: "var(--th-header-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="mb-1.5 px-0.5 text-[10px] font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          Workspace color
        </div>
        <div className="grid grid-cols-4 gap-1.5">
          {WORKSPACE_COLOR_PALETTE.map((c) => {
            const selected =
              !!current && current.toLowerCase() === c.toLowerCase();
            return (
              <button
                key={c}
                type="button"
                onClick={() => onPick(c)}
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
