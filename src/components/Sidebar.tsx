// The 0.5 sidebar — the daily supervision surface (PLAN.md §F, read-only for
// 0.5). Four stacked areas:
//   1. Attention queue: sessions wanting input (question/permission/failure),
//      rate-limited, or a freshly completed main turn. Clicking a row calls
//      onSelectSession.
//   2. Session/supervision tree: every supervised orchestrator with its
//      subagent children + outstanding task count (the headline 0.5 view), each
//      with a compact context-usage line from the statusline snapshot.
//   3. Hooks: the consent-gated Claude hook install/uninstall control.
//   4. Utility area: compact WSL health + agent connection state (low priority).
//
// Composes the presentational components + the supervision store + the
// telemetry hook. No direct IPC beyond the hook + the install panel. Inert when
// there's no agent data yet (muted "no sessions"/"pending" states). All live
// data arrives via the agent bridge's event emit spine (agent://journal →
// supervision://tree / session://status / status://snapshot); the telemetry hook
// subscribes and feeds the store.
import { useMemo, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  useSupervision,
  attentionSessions,
  displayStatus,
} from "../store/supervision";
import { useAgentTelemetry } from "../store/telemetry";
import { useSettings } from "../store/settings";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { startPointerDrag } from "../lib/pointerDrag";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { SupervisionTreeBody } from "./SupervisionTree";
import { StatusBadge, statusLabel } from "./StatusBadge";
import { WslHealth } from "./WslHealth";
import { HookInstallPanel } from "./HookInstallPanel";
import type { StatusSnapshot, SupervisionTree } from "../ipc/model";
import type {
  TerminalId,
  TerminalInfo,
  TerminalState,
} from "../ipc/types";

// --- Frameless window controls -------------------------------------------
// The brand + settings gear + window controls (minimize / maximize-restore /
// close) now live at the TOP of the sidebar instead of the titlebar, so the
// titlebar can be a clean tab-strip row. These call the Tauri window API and
// MUST NOT carry data-tauri-drag-region, or a click would start a window drag.
// (The titlebar keeps an identical fallback cluster for the sidebar-hidden
// state — see Titlebar.tsx — so the controls are never unreachable.)

/** Minimize the window, swallowing any IPC rejection. */
function minimizeWindow(): void {
  void getCurrentWindow()
    .minimize()
    .catch(() => {});
}

/** Toggle maximize/restore, swallowing any IPC rejection. */
function toggleMaximizeWindow(): void {
  void getCurrentWindow()
    .toggleMaximize()
    .catch(() => {});
}

/** Close the window, swallowing any IPC rejection. */
function closeWindow(): void {
  void getCurrentWindow()
    .close()
    .catch(() => {});
}

/** The workspace-row id under a viewport point, or null (drag resolution). Each
 *  WorkspaceRow header carries `data-ws-id`; elementFromPoint returns the topmost
 *  element under the pointer, so we walk up to the owning row with `closest`. Used
 *  by BOTH the workspace-reorder drag and the cross-workspace terminal drag. */
function workspaceUnder(x: number, y: number): string | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  return (
    el?.closest<HTMLElement>("[data-ws-id]")?.getAttribute("data-ws-id") ?? null
  );
}

/**
 * Lifecycle-dot color per terminal state, mirroring Tile.tsx's DOT_VAR so the
 * per-workspace terminal list (#2) reads the same themed `--th-dot-*` palette
 * (amber=starting / green=live / gray=detached / dim=exited / red=error).
 */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

/**
 * The sidebar's 3-state collapse mode (App owns + persists it; #1):
 *  - "full": the resizable full-width supervision surface (the original view).
 *  - "rail": a thin ~48px strip showing just iconic section markers + a compact
 *    workspace list, "barely showing" but still useful for switching tabs.
 *  - "hidden": not rendered at all (App skips <Sidebar> entirely).
 * The cycle full -> rail -> hidden -> full is driven by App's onToggleSidebar.
 */
export type SidebarMode = "full" | "rail" | "hidden";

/** Pixel width of the rail strip (kept in sync with App's RAIL width). */
export const SIDEBAR_RAIL_WIDTH = 48;

export interface SidebarProps {
  /** Called when the user clicks an attention-queue row or a tree header. */
  onSelectSession?: (sessionId: string) => void;
  /** Collapse mode (#1). "hidden" is handled by App (it skips render), so the
   *  component itself only ever sees "full" or "rail"; defaults to "full". */
  mode?: SidebarMode;
  /** Sidebar width in px (resizable, #2). Defaults to 256 (the old fixed w-64). */
  width?: number;
  /**
   * Cycle the sidebar collapse state (full -> rail -> hidden -> full). The
   * sidebar header's collapse button uses this so the chrome that now lives in
   * the sidebar can drive the same Ctrl/Cmd+B cycle App owns.
   */
  onToggleSidebar?: () => void;
  /**
   * Resolved path to the termhub-agent binary used as the hook entrypoint
   * (`<agentBin> --hook <EVENT>`). Inside WSL `termhub-agent` is on PATH, so the
   * bare name is the right default; a dev box can override it.
   */
  agentBin?: string;
}

export function Sidebar({
  onSelectSession,
  mode = "full",
  width = 256,
  onToggleSidebar,
  agentBin = "termhub-agent",
}: SidebarProps) {
  // Workspace tabs (read-only, #2): list every tab with its tile count and let
  // a click activate it. Each tab also expands to its terminals (looked up in
  // the live `terminals` map) so the user can peek into OTHER workspaces without
  // switching. We never mutate the store beyond setActiveTab / setFocus.
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const terminals = useWorkspace((s) => s.terminals);
  const setFocus = useWorkspace((s) => s.setFocus);

  // Rail mode: a thin, iconic strip. Render before pulling the heavier
  // supervision selectors below stays cheap, but hooks must run unconditionally,
  // so derive everything and branch on render.
  if (mode === "rail") {
    return (
      <SidebarRail
        width={width}
        tabs={tabs}
        activeTabId={activeTabId}
        setActiveTab={setActiveTab}
        onToggleSidebar={onToggleSidebar}
      />
    );
  }
  return (
    <SidebarFull
      onSelectSession={onSelectSession}
      width={width}
      agentBin={agentBin}
      tabs={tabs}
      activeTabId={activeTabId}
      setActiveTab={setActiveTab}
      terminals={terminals}
      setFocus={setFocus}
      onToggleSidebar={onToggleSidebar}
    />
  );
}

/**
 * The Workspaces section's pointer-drag actions, pulled together so WorkspaceList
 * can drive both interactions (reorder a workspace, move a terminal across
 * workspaces) and the inline rename. These reuse the EXISTING store actions +
 * the SAME transient drag fields the titlebar/tile drags use, so the sidebar's
 * drop highlighting stays consistent with the rest of the app.
 */
function useWorkspaceDragActions() {
  return {
    moveTab: useWorkspace((s) => s.moveTab),
    renameTab: useWorkspace((s) => s.renameTab),
    moveTileToTab: useWorkspace((s) => s.moveTileToTab),
    setDraggingTab: useWorkspace((s) => s.setDraggingTab),
    setDraggingTile: useWorkspace((s) => s.setDraggingTile),
    setDropTab: useWorkspace((s) => s.setDropTab),
  };
}

interface FullProps {
  onSelectSession?: (sessionId: string) => void;
  width: number;
  agentBin: string;
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
  terminals: Record<TerminalId, TerminalInfo>;
  setFocus: (id: TerminalId) => void;
  onToggleSidebar?: () => void;
}

function SidebarFull({
  onSelectSession,
  width,
  agentBin,
  tabs,
  activeTabId,
  setActiveTab,
  terminals,
  setFocus,
  onToggleSidebar,
}: FullProps) {
  const { metrics, agent } = useAgentTelemetry();
  const trees = useSupervision((s) => s.trees);
  const statuses = useSupervision((s) => s.statuses);
  const snapshots = useSupervision((s) => s.snapshots);

  // Apply the rate-limit overlay (FR-012 `rateLimited` is a statusline overlay,
  // not a reducer state) to every known session's status before deriving the
  // queue and rendering badges, so a near-cap session reads as rate-limited.
  const displayStatuses = useMemo(() => {
    const out: Record<string, (typeof statuses)[string]> = {};
    for (const [sid, st] of Object.entries(statuses)) {
      out[sid] = displayStatus(st, snapshots[sid]);
    }
    return out;
  }, [statuses, snapshots]);

  const treeList = Object.values(trees).sort((a, b) =>
    a.sessionId.localeCompare(b.sessionId),
  );
  const queue = attentionSessions(displayStatuses);

  return (
    <aside
      className="flex h-full shrink-0 flex-col border-r"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {/* Top chrome header (moved out of the titlebar): the T-Hub brand on the
          left; the settings gear + window controls (min / max-restore / close)
          on the right. The brand area is a window-drag handle. */}
      <SidebarHeader onToggleSidebar={onToggleSidebar} />

      {/* 0. Workspaces (#2) — the user's tabs with tile counts, active one
          highlighted; a click activates the tab. Each row expands to that tab's
          terminals so the user can peek into OTHER workspaces without switching.
          Gives the otherwise-empty sidebar immediate utility. */}
      <section
        className="border-b"
        style={{ borderColor: "var(--th-border)" }}
      >
        <Header>Workspaces</Header>
        <WorkspaceList
          tabs={tabs}
          activeTabId={activeTabId}
          setActiveTab={setActiveTab}
          terminals={terminals}
          setFocus={setFocus}
        />
      </section>

      {/* 1. Attention queue */}
      <section
        className="border-b"
        style={{ borderColor: "var(--th-border)" }}
      >
        <Header>Attention</Header>
        {queue.length === 0 ? (
          <Muted>Nothing needs you.</Muted>
        ) : (
          <ul>
            {queue.map(({ sessionId, status }) => (
              <li key={sessionId}>
                <button
                  type="button"
                  onClick={() => onSelectSession?.(sessionId)}
                  className="flex w-full items-center gap-2 px-2 py-1 text-left text-sm hover:bg-neutral-900"
                  title={`${statusLabel(status)} — ${sessionId}`}
                >
                  <StatusBadge status={status} dotOnly />
                  <span
                    className="min-w-0 flex-1 truncate"
                    style={{ color: "var(--th-fg)" }}
                  >
                    {sessionId}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* 2. Session / supervision tree (scrolls) */}
      <section className="min-h-0 flex-1 overflow-y-auto">
        <Header>Sessions</Header>
        {treeList.length === 0 ? (
          <Muted>No supervised sessions.</Muted>
        ) : (
          <div className="divide-y divide-neutral-800">
            {treeList.map((tree) => (
              <SessionRow
                key={tree.sessionId}
                tree={tree}
                displayStatus={displayStatuses[tree.sessionId] ?? tree.status}
                snapshot={snapshots[tree.sessionId]}
                onSelect={() => onSelectSession?.(tree.sessionId)}
              />
            ))}
          </div>
        )}
      </section>

      {/* 3. Claude hooks (consent-gated install/uninstall) */}
      <section
        className="border-t"
        style={{ borderColor: "var(--th-border)" }}
      >
        <HookInstallPanel agentBin={agentBin} />
      </section>

      {/* 4. Utility area (low priority) */}
      <section
        className="border-t"
        style={{ borderColor: "var(--th-border)" }}
      >
        <Header>WSL</Header>
        <WslHealth metrics={metrics} connection={agent?.connection} />
      </section>
    </aside>
  );
}

/** The Workspaces list (full mode): one expandable row per tab = name + tile
 *  count, the active tab accent-highlighted. Clicking the row activates it via
 *  setActiveTab; the chevron toggles an inline list of that tab's terminals so
 *  the user can see what's in OTHER workspaces without switching (#2).
 *
 *  Three pointer-based operations live here, all reusing existing store actions:
 *   - drag a workspace row up/down to reorder it (moveTab);
 *   - double-click a row's name to rename it inline (renameTab);
 *   - drag a TerminalRow onto a DIFFERENT workspace to move it there
 *     (moveTileToTab).
 *  The inline-rename draft and the live drop-target id are lifted here so every
 *  row shares them (only one row is ever editing / highlighted at a time). The
 *  drop highlight reuses the store's transient drag fields (the same the titlebar
 *  /tile drags use) for app-wide consistency. */
function WorkspaceList({
  tabs,
  activeTabId,
  setActiveTab,
  terminals,
  setFocus,
}: {
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
  terminals: Record<TerminalId, TerminalInfo>;
  setFocus: (id: TerminalId) => void;
}) {
  const actions = useWorkspaceDragActions();
  // The drag source (a workspace being reordered, or a terminal being moved) and
  // the row currently under the pointer — read live for drop-target highlighting,
  // mirroring the titlebar/tile drags. The drag source itself is never a target.
  const draggingTabId = useWorkspace((s) => s.draggingTabId);
  const draggingTileId = useWorkspace((s) => s.draggingTileId);
  const dropWsId = useWorkspace((s) => s.dropTabId);

  // id of the workspace whose name is being renamed inline (null = none).
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  // Set true the instant a drag commits so the synthetic click that fires on
  // pointerup (over the drag-source row/terminal) is swallowed — otherwise a
  // committed reorder/move would ALSO activate/focus the source. Cleared on the
  // next pointerdown. (A plain click never sets it, so click-to-activate works.)
  const suppressClickRef = useRef(false);

  const startRename = (id: string, name: string) => {
    setEditing(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (editing) actions.renameTab(editing, draft);
    setEditing(null);
  };

  // Reorder a workspace by dragging its header up/down. A plain press (under the
  // helper's threshold) never reorders, so click-to-activate still works; the
  // row under release resolves via elementFromPoint + [data-ws-id].
  const onRowPointerDown = (tabId: string, e: ReactPointerEvent) => {
    if (editing === tabId) return; // let the rename input own the pointer
    if (e.button !== 0) return;
    suppressClickRef.current = false;
    // Ghost details captured at press time (the workspace's name + tile count) so
    // the floating chip matches the titlebar/tile drags' "I'm carrying this" cue.
    const tab = tabs.find((t) => t.id === tabId);
    const wsCount = tab?.order.length ?? 0;
    const wsName = `${tab?.name ?? "Workspace"} · ${wsCount} terminal${
      wsCount === 1 ? "" : "s"
    }`;
    let ghost: DragGhost | null = null;
    startPointerDrag(e.clientX, e.clientY, {
      onBegin: () => {
        actions.setDraggingTab(tabId);
        document.body.dataset.thDragging = "1";
        // Header-only chip (bodyHeight 0), like the titlebar tab ghost; fold the
        // tile count into the title so it shows on the single-line chip.
        ghost = createDragGhost({ title: wsName, width: 160, bodyHeight: 0 });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        const overId = workspaceUnder(x, y);
        actions.setDropTab(overId && overId !== tabId ? overId : null);
      },
      onEnd: (x, y, committed) => {
        const targetId = committed ? workspaceUnder(x, y) : null;
        ghost?.destroy();
        ghost = null;
        delete document.body.dataset.thDragging;
        actions.setDraggingTab(null);
        actions.setDropTab(null);
        if (!committed) return;
        // A real drag happened: swallow the trailing click so the source row
        // isn't activated by it.
        suppressClickRef.current = true;
        if (targetId && targetId !== tabId) {
          actions.moveTab(tabId, targetId);
        }
      },
    });
  };

  // Drag a terminal onto a DIFFERENT workspace row to move it there. Dropping on
  // its OWN workspace (or off any row) is a no-op; the target resolves the same
  // way as the workspace reorder (elementFromPoint + [data-ws-id]).
  const onTerminalPointerDown = (
    terminalId: TerminalId,
    ownTabId: string,
    e: ReactPointerEvent,
  ) => {
    if (e.button !== 0) return;
    suppressClickRef.current = false;
    // Ghost details captured at press time: the terminal's title + lifecycle
    // state, mirroring the TerminalRow tooltip (`${title} — ${state}`).
    const info = terminals[terminalId];
    const termTitle = info?.title?.trim() || terminalId;
    const termState = info?.state ?? "starting";
    let ghost: DragGhost | null = null;
    startPointerDrag(e.clientX, e.clientY, {
      onBegin: () => {
        actions.setDraggingTile(terminalId);
        document.body.dataset.thDragging = "1";
        // Small chip with a faded subtitle line showing the lifecycle state.
        ghost = createDragGhost({
          title: termTitle,
          subtitle: termState,
          width: 180,
          bodyHeight: 22,
        });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        const overId = workspaceUnder(x, y);
        actions.setDropTab(overId && overId !== ownTabId ? overId : null);
      },
      onEnd: (x, y, committed) => {
        const targetId = committed ? workspaceUnder(x, y) : null;
        ghost?.destroy();
        ghost = null;
        delete document.body.dataset.thDragging;
        actions.setDraggingTile(null);
        actions.setDropTab(null);
        if (!committed) return;
        // A real drag happened: swallow the trailing click so it doesn't focus
        // the terminal / activate its source tab.
        suppressClickRef.current = true;
        if (targetId && targetId !== ownTabId) {
          actions.moveTileToTab(terminalId, targetId);
        }
      },
    });
  };

  // True when the just-finished gesture was a committed drag, so the row/terminal
  // click handlers can no-op the synthetic click that immediately follows.
  const consumeSuppressedClick = (): boolean => {
    if (!suppressClickRef.current) return false;
    suppressClickRef.current = false;
    return true;
  };

  if (tabs.length === 0) return <Muted>No workspaces.</Muted>;
  return (
    <ul>
      {tabs.map((tab) => (
        <WorkspaceRow
          key={tab.id}
          tab={tab}
          active={tab.id === activeTabId}
          setActiveTab={setActiveTab}
          terminals={terminals}
          setFocus={setFocus}
          // A live drop target by EITHER a workspace reorder or a terminal being
          // dragged onto it; never the drag source itself.
          isDropTarget={
            dropWsId === tab.id &&
            draggingTabId !== tab.id
          }
          // Dim the workspace currently being reordered (matches the tab strip).
          isDragging={draggingTabId === tab.id}
          // Suppress the row's click-to-activate while a terminal drag is in
          // flight so releasing over a row doesn't also switch to it.
          dragInProgress={draggingTileId != null}
          editing={editing === tab.id}
          draft={draft}
          onDraftChange={setDraft}
          onStartRename={() => startRename(tab.id, tab.name)}
          onCommitRename={commitRename}
          onCancelRename={() => setEditing(null)}
          onRowPointerDown={onRowPointerDown}
          onTerminalPointerDown={onTerminalPointerDown}
          consumeSuppressedClick={consumeSuppressedClick}
        />
      ))}
    </ul>
  );
}

/** One workspace row: a header (chevron + name + tile count, click activates the
 *  tab) plus a collapsible list of the tab's terminals. The active workspace
 *  defaults expanded; the rest start collapsed (local useState, #2). The header
 *  carries `data-ws-id` so it resolves as a drop target for the workspace reorder
 *  and the cross-workspace terminal drag. */
function WorkspaceRow({
  tab,
  active,
  setActiveTab,
  terminals,
  setFocus,
  isDropTarget,
  isDragging,
  dragInProgress,
  editing,
  draft,
  onDraftChange,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onRowPointerDown,
  onTerminalPointerDown,
  consumeSuppressedClick,
}: {
  tab: WorkspaceTab;
  active: boolean;
  setActiveTab: (id: string) => void;
  terminals: Record<TerminalId, TerminalInfo>;
  setFocus: (id: TerminalId) => void;
  isDropTarget: boolean;
  isDragging: boolean;
  dragInProgress: boolean;
  editing: boolean;
  draft: string;
  onDraftChange: (v: string) => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onRowPointerDown: (tabId: string, e: ReactPointerEvent) => void;
  onTerminalPointerDown: (
    terminalId: TerminalId,
    ownTabId: string,
    e: ReactPointerEvent,
  ) => void;
  consumeSuppressedClick: () => boolean;
}) {
  const count = tab.order.length;
  // Active workspace starts open; the others collapse so the list stays compact.
  const [expanded, setExpanded] = useState(active);

  return (
    <li>
      <div
        // data-ws-id: the drop target a workspace reorder / a terminal drag
        // resolves to via elementFromPoint + closest.
        data-ws-id={tab.id}
        className={`flex w-full items-center hover:bg-neutral-900 ${
          isDragging ? "opacity-40" : ""
        }`}
        style={{
          color: "var(--th-fg)",
          ...(active
            ? { backgroundColor: "var(--th-accent)" }
            : {}),
          // A subtle themed drop indicator: an accent inset ring on the row the
          // pointer is over, matching the tab strip / tile drop styling.
          ...(isDropTarget
            ? { boxShadow: "inset 0 0 0 1px var(--th-accent)" }
            : {}),
        }}
      >
        {/* Chevron toggle — expand/collapse without switching workspaces. */}
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          className="flex h-6 w-5 shrink-0 items-center justify-center text-[10px] leading-none opacity-70 hover:opacity-100"
          aria-label={expanded ? "Collapse workspace" : "Expand workspace"}
          aria-expanded={expanded}
          title={expanded ? "Collapse" : "Expand"}
        >
          {expanded ? "v" : ">"}
        </button>
        {/* Name + count — pressing activates the tab; dragging reorders it
            (pointer-based, threshold-gated so a plain click still activates).
            Double-clicking the name renames it inline. While a terminal drag is
            in flight, suppress the activate so releasing over a row only moves
            the terminal. */}
        {editing ? (
          <input
            autoFocus
            value={draft}
            onChange={(e) => onDraftChange(e.target.value)}
            onBlur={onCommitRename}
            onPointerDown={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Enter") onCommitRename();
              else if (e.key === "Escape") onCancelRename();
            }}
            className="my-0.5 mr-2 min-w-0 flex-1 bg-neutral-700 px-1 text-sm text-neutral-100 outline-none"
            style={{ boxShadow: "0 0 0 1px var(--th-accent)" }}
          />
        ) : (
          <button
            type="button"
            onPointerDown={(e) => onRowPointerDown(tab.id, e)}
            onClick={() => {
              // Swallow the click that trails a committed reorder; a terminal
              // drag in flight also shouldn't activate the source tab.
              if (consumeSuppressedClick() || dragInProgress) return;
              setActiveTab(tab.id);
            }}
            onDoubleClick={onStartRename}
            className="flex min-w-0 flex-1 cursor-pointer touch-none select-none items-center gap-2 py-1 pr-2 text-left text-sm"
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"}`}
            aria-current={active ? "true" : undefined}
          >
            <span className="min-w-0 flex-1 truncate">{tab.name}</span>
            <span className="shrink-0 tabular-nums opacity-70">{count}</span>
          </button>
        )}
      </div>
      {expanded && (
        <ul className="pb-1">
          {count === 0 ? (
            <li
              className="px-2 py-0.5 pl-7 text-xs"
              style={{ color: "var(--th-fg-muted)" }}
            >
              No terminals.
            </li>
          ) : (
            tab.order.map((id) => (
              <TerminalRow
                key={id}
                id={id}
                info={terminals[id]}
                onClick={() => {
                  // A committed cross-workspace drag swallows its trailing click.
                  if (consumeSuppressedClick()) return;
                  setActiveTab(tab.id);
                  setFocus(id);
                }}
                onPointerDown={(e) => onTerminalPointerDown(id, tab.id, e)}
              />
            ))
          )}
        </ul>
      )}
    </li>
  );
}

/** One terminal under a workspace: a themed lifecycle dot + the terminal title.
 *  Clicking activates the owning tab and focuses this tile (#2). Dragging it onto
 *  a DIFFERENT workspace row moves it there (pointer-based, threshold-gated so a
 *  plain click still focuses). The record may be missing if the live map hasn't
 *  seeded that id yet -- fall back gracefully. */
function TerminalRow({
  id,
  info,
  onClick,
  onPointerDown,
}: {
  id: TerminalId;
  info?: TerminalInfo;
  onClick: () => void;
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  const state: TerminalState = info?.state ?? "starting";
  const title = info?.title?.trim() || id;
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        onPointerDown={onPointerDown}
        className="flex w-full cursor-pointer touch-none items-center gap-2 py-0.5 pr-2 pl-7 text-left text-xs hover:bg-neutral-900"
        style={{ color: "var(--th-fg-muted)" }}
        title={`${title} — ${state}`}
      >
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: DOT_VAR[state] }}
          aria-hidden
        />
        <span className="min-w-0 flex-1 truncate">{title}</span>
      </button>
    </li>
  );
}

/**
 * Rail mode (#1): a thin ~48px iconic strip — "barely showing" but still useful.
 * It stacks one square per workspace tab (its initial + a tiny tile count) so the
 * user can still switch tabs, then a small column of section glyphs (Attention /
 * Sessions / Hooks / WSL) as a hint of what the full sidebar holds.
 */
function SidebarRail({
  width,
  tabs,
  activeTabId,
  setActiveTab,
  onToggleSidebar,
}: {
  width: number;
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
  onToggleSidebar?: () => void;
}) {
  return (
    <aside
      className="flex h-full shrink-0 flex-col items-center gap-1 border-r"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {/* Compact header for the rail: the brand mark (also a drag handle) stacked
          over the window controls, so the chrome is reachable even in the thin
          strip. The collapse button cycles to full/hidden. */}
      <SidebarRailHeader onToggleSidebar={onToggleSidebar} />
      <div className="flex flex-col items-center gap-1 pt-1">
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        const count = tab.order.length;
        const initial = (tab.name.trim()[0] ?? "?").toUpperCase();
        return (
          <button
            key={tab.id}
            type="button"
            onClick={() => setActiveTab(tab.id)}
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"}`}
            aria-current={active ? "true" : undefined}
            className="relative flex h-8 w-8 items-center justify-center rounded text-xs font-semibold hover:opacity-90"
            style={{
              backgroundColor: active ? "var(--th-accent)" : "transparent",
              color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
              border: active ? undefined : "1px solid var(--th-border)",
            }}
          >
            {initial}
            {count > 0 && (
              <span
                className="absolute -right-0.5 -top-0.5 min-w-[12px] rounded-full px-0.5 text-center text-[8px] leading-[12px]"
                style={{
                  backgroundColor: "var(--th-border)",
                  color: "var(--th-fg)",
                }}
              >
                {count}
              </span>
            )}
          </button>
        );
      })}
      {tabs.length === 0 && (
        <div
          className="text-xs"
          style={{ color: "var(--th-fg-muted)" }}
          title="No workspaces"
        >
          —
        </div>
      )}
      </div>
      {/* Section hints: glyphs standing in for the full sidebar's sections. */}
      <div
        className="mt-auto flex flex-col items-center gap-1 px-1 pb-2 pt-2 text-sm"
        style={{ color: "var(--th-fg-muted)" }}
        aria-hidden
      >
        <span title="Attention">!</span>
        <span title="Sessions">≡</span>
        <span title="Hooks">⚓</span>
        <span title="WSL">◷</span>
      </div>
    </aside>
  );
}

// ===========================================================================
// Sidebar chrome header — the window's top-left chrome, relocated here from the
// titlebar. FULL mode: a single 32px row with the T-Hub brand on the left and
// the settings gear + window controls on the right. RAIL mode: a compact
// stacked version. When the sidebar is HIDDEN entirely, App skips <Sidebar>, so
// the titlebar renders a fallback control cluster instead (see Titlebar.tsx) —
// the controls are therefore always reachable.
// ===========================================================================

/**
 * The full-mode sidebar header: a 32px row matching the titlebar height. The
 * left holds the brand (a window-drag handle); the right holds a collapse
 * button (cycles full -> rail -> hidden), the settings gear, and the window
 * controls (minimize / maximize-restore / close). The empty middle is also a
 * drag handle so the window can still be moved by grabbing the header.
 */
function SidebarHeader({ onToggleSidebar }: { onToggleSidebar?: () => void }) {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  return (
    <div
      className="flex h-8 shrink-0 items-stretch border-b"
      style={{ borderColor: "var(--th-border)" }}
    >
      <SidebarBrand />
      {/* Draggable filler so the header itself moves the window. */}
      <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />
      {onToggleSidebar && (
        <CollapseButton onClick={onToggleSidebar} />
      )}
      <SidebarSettingsButton onClick={toggleSettings} />
      <SidebarWindowControls />
    </div>
  );
}

/**
 * The rail-mode header: the brand mark and window controls stacked vertically so
 * they fit the thin (~48px) strip. The brand square doubles as a window-drag
 * handle; the collapse button expands the rail back to full (or on to hidden).
 */
function SidebarRailHeader({ onToggleSidebar }: { onToggleSidebar?: () => void }) {
  return (
    <div
      className="flex w-full flex-col items-center gap-1 border-b pb-1.5 pt-1.5"
      style={{ borderColor: "var(--th-border)" }}
    >
      {/* Only the brand mark is a drag handle; the control buttons must NOT be
          inside a drag region or a click would start a window drag instead. */}
      <span
        data-tauri-drag-region
        className="inline-block h-3 w-3 rounded-[2px]"
        style={{ backgroundColor: "var(--th-accent)" }}
        title="T-Hub"
        aria-hidden
      />
      {onToggleSidebar && (
        <button
          type="button"
          onClick={onToggleSidebar}
          aria-label="Expand sidebar"
          title="Expand sidebar"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-300 transition-colors hover:bg-neutral-700"
        >
          <SidebarToggleIcon />
        </button>
      )}
      <button
        type="button"
        onClick={minimizeWindow}
        aria-label="Minimize"
        title="Minimize"
        className="flex h-6 w-6 items-center justify-center rounded text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <MinimizeIcon />
      </button>
      <button
        type="button"
        onClick={toggleMaximizeWindow}
        aria-label="Maximize"
        title="Maximize / Restore"
        className="flex h-6 w-6 items-center justify-center rounded text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <MaximizeIcon />
      </button>
      <button
        type="button"
        onClick={closeWindow}
        aria-label="Close"
        title="Close"
        className="flex h-6 w-6 items-center justify-center rounded text-neutral-300 transition-colors hover:bg-red-600 hover:text-white"
      >
        <CloseIcon />
      </button>
    </div>
  );
}

/** "T-Hub" wordmark with a small accent glyph; a window-drag handle. */
function SidebarBrand() {
  return (
    <div
      data-tauri-drag-region
      className="flex shrink-0 select-none items-center gap-1.5 pl-2.5 pr-2"
    >
      <span
        className="inline-block h-2.5 w-2.5 rounded-[2px]"
        style={{ backgroundColor: "var(--th-accent)" }}
        aria-hidden
      />
      <span
        className="text-xs font-semibold tracking-tight"
        style={{ color: "var(--th-fg)" }}
      >
        T-Hub
      </span>
    </div>
  );
}

/** Collapse button — cycles the sidebar (full -> rail -> hidden). */
function CollapseButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Collapse sidebar"
      title="Collapse sidebar (Ctrl/Cmd+B)"
      className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
      <SidebarToggleIcon />
    </button>
  );
}

/** Settings gear — opens the settings/theme surface (also Ctrl/Cmd+,). */
function SidebarSettingsButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      aria-label="Settings"
      title="Settings (Ctrl/Cmd+,)"
      onClick={onClick}
      className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
      <GearIcon />
    </button>
  );
}

/** Window controls (minimize / maximize-restore / close) for the full header. */
function SidebarWindowControls() {
  return (
    <div className="flex shrink-0 items-stretch">
      <button
        type="button"
        aria-label="Minimize"
        title="Minimize"
        onClick={minimizeWindow}
        className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <MinimizeIcon />
      </button>
      <button
        type="button"
        aria-label="Maximize"
        title="Maximize / Restore"
        onClick={toggleMaximizeWindow}
        className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <MaximizeIcon />
      </button>
      <button
        type="button"
        aria-label="Close"
        title="Close"
        onClick={closeWindow}
        className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-red-600 hover:text-white"
      >
        <CloseIcon />
      </button>
    </div>
  );
}

// --- Shared chrome icons (sized to sit in the 32px header) -----------------

/** Settings gear. */
function GearIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/** Minimize (horizontal line). */
function MinimizeIcon() {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 10 10"
      aria-hidden
      className="pointer-events-none"
    >
      <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

/** Maximize / restore (square). */
function MaximizeIcon() {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 10 10"
      aria-hidden
      className="pointer-events-none"
    >
      <rect
        x="1"
        y="1"
        width="8"
        height="8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1"
      />
    </svg>
  );
}

/** Close (×). */
function CloseIcon() {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 10 10"
      aria-hidden
      className="pointer-events-none"
    >
      <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" strokeWidth="1" />
      <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

/** Sidebar collapse/expand glyph (a panel with a divider). */
function SidebarToggleIcon() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <line x1="9" y1="4" x2="9" y2="20" />
    </svg>
  );
}

/** One orchestrator row: the supervision tree body (with the overlaid status)
 *  plus a compact statusline-usage line when a snapshot exists. */
function SessionRow({
  tree,
  displayStatus,
  snapshot,
  onSelect,
}: {
  tree: SupervisionTree;
  displayStatus: SupervisionTree["status"];
  snapshot?: StatusSnapshot;
  onSelect: () => void;
}) {
  // Render the tree with the rate-limit-overlaid status so the badge matches the
  // attention queue (the tree's own status field is the raw reducer status).
  const overlaid: SupervisionTree = { ...tree, status: displayStatus };
  return (
    <button
      type="button"
      onClick={onSelect}
      className="block w-full text-left hover:bg-neutral-900/50"
    >
      <SupervisionTreeBody tree={overlaid} label={tree.sessionId} />
      {snapshot && <UsageLine snapshot={snapshot} />}
    </button>
  );
}

/** A dense one-liner: context %, cost, and the nearest rate-limit window %. */
function UsageLine({ snapshot }: { snapshot: StatusSnapshot }) {
  const ctx = snapshot.contextUsedPct;
  const cost = snapshot.costUsd;
  const rl = Math.max(
    snapshot.fiveHour?.usedPercentage ?? 0,
    snapshot.sevenDay?.usedPercentage ?? 0,
  );
  const parts: string[] = [];
  if (ctx != null) parts.push(`ctx ${ctx.toFixed(0)}%`);
  if (snapshot.rateLimitsPresent && rl > 0) parts.push(`rl ${rl.toFixed(0)}%`);
  if (cost != null) parts.push(`$${cost.toFixed(2)}`);
  if (parts.length === 0) return null;
  return (
    <div
      className="px-2 pb-1 pl-4 text-xs"
      style={{ color: "var(--th-fg-muted)" }}
    >
      {parts.join(" · ")}
    </div>
  );
}

function Header({ children }: { children: React.ReactNode }) {
  return (
    <div
      className="px-2 pt-2 pb-1 text-xs font-semibold uppercase tracking-wide"
      style={{ color: "var(--th-fg-muted)" }}
    >
      {children}
    </div>
  );
}

function Muted({ children }: { children: React.ReactNode }) {
  return (
    <div
      className="px-2 py-1 text-sm"
      style={{ color: "var(--th-fg-muted)" }}
    >
      {children}
    </div>
  );
}
