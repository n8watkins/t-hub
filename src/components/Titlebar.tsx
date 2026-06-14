// Persistent Chrome-style top bar for the frameless (decorations:false) main
// window — the ONLY window chrome (shell v2).
//
// Layout, left -> right:
//   [short draggable region] · [workspace tab strip + "＋" new-tab button at the
//   right of the right-most tab] · [flexible draggable region] · [window controls]
//
// The draggable regions carry `data-tauri-drag-region`, so the user can grab the
// empty areas beside the tabs to move the window (just like Chrome). The bar is
// always visible (~32px) with a subtle 1px bottom border, and it participates in
// layout (App renders it above the body row) so nothing overlaps the canvas.
//
// Window controls (minimize / maximize-restore / close) use the Tauri window API
// and must NOT carry data-tauri-drag-region, or a click would start a window
// drag instead. (Custom controls can't reproduce the Win11 maximize-hover snap
// menu — an accepted tradeoff for a frameless, real-estate-frugal window.)
import { useState } from "react";
import type { DragEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useWorkspace } from "../store/workspace";

/** Minimize the window, swallowing any IPC rejection. */
function minimize(): void {
  void getCurrentWindow()
    .minimize()
    .catch(() => {});
}

/** Toggle maximize/restore, swallowing any IPC rejection. */
function toggleMaximize(): void {
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

/** dataTransfer MIME used to carry the dragged tab's id during reorder. */
const TAB_DND_MIME = "application/x-termhub-tab";

export function Titlebar() {
  return (
    <div className="flex h-8 shrink-0 items-stretch border-b border-neutral-800 bg-neutral-900 text-xs">
      {/* Short left drag handle so the window can be grabbed left of the tabs. */}
      <div data-tauri-drag-region className="w-3 shrink-0" aria-hidden />

      {/* Workspace tabs (+ the new-tab button at the right of the last tab). */}
      <TabStrip />

      {/* Flexible drag region: dragging this empty stretch moves the window. */}
      <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />

      {/* Window controls (top-right). No drag-region, or clicks would drag. */}
      <WindowControls />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Window controls: minimize, maximize/restore, close. Fixed-width hover targets
// matching the bar height; close goes red on hover.
// ---------------------------------------------------------------------------
function WindowControls() {
  return (
    <div className="flex shrink-0 items-stretch">
      <button
        type="button"
        aria-label="Minimize"
        title="Minimize"
        onClick={minimize}
        className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <svg
          width="10"
          height="10"
          viewBox="0 0 10 10"
          aria-hidden
          className="pointer-events-none"
        >
          <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button
        type="button"
        aria-label="Maximize"
        title="Maximize / Restore"
        onClick={toggleMaximize}
        className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
      >
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
      </button>
      <button
        type="button"
        aria-label="Close"
        title="Close"
        onClick={closeWindow}
        className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-red-600 hover:text-white"
      >
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
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Workspace tab strip (PRD §5.2), now hosted in the top bar. Click to activate,
// double-click to rename inline, × to close an empty tab, and drag a tab
// left/right to reorder it. The "＋" new-tab button sits immediately to the
// right of the right-most tab.
// ---------------------------------------------------------------------------
function TabStrip() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const addTab = useWorkspace((s) => s.addTab);
  const renameTab = useWorkspace((s) => s.renameTab);
  const closeTab = useWorkspace((s) => s.closeTab);
  const moveTab = useWorkspace((s) => s.moveTab);

  // id of the tab currently being renamed inline (null = none).
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  // id of the tab currently being dragged (for dim) + the drop-target tab.
  const [dragging, setDragging] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  const startRename = (id: string, name: string) => {
    setEditing(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (editing) renameTab(editing, draft);
    setEditing(null);
  };

  const isTabDrag = (e: DragEvent) =>
    e.dataTransfer.types.includes(TAB_DND_MIME);

  return (
    // The strip scrolls horizontally if there are many tabs; it never grows past
    // the available width, so the flexible drag region + controls stay reachable.
    // `overflow-y-hidden` clips the scrollbar gutter so it can't steal the row.
    <div className="flex min-w-0 items-stretch gap-1 overflow-x-auto overflow-y-hidden px-1">
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        const closable = tabs.length > 1 && tab.order.length === 0;
        const isDropTarget = dropTarget === tab.id && dragging !== tab.id;
        return (
          <div
            key={tab.id}
            // The whole tab is a drag handle for reorder. While renaming, drags
            // are disabled so text selection in the input works.
            draggable={editing !== tab.id}
            onDragStart={(e) => {
              e.dataTransfer.setData(TAB_DND_MIME, tab.id);
              e.dataTransfer.setData("text/plain", tab.name);
              e.dataTransfer.effectAllowed = "move";
              setDragging(tab.id);
            }}
            onDragEnd={() => {
              setDragging(null);
              setDropTarget(null);
            }}
            onDragOver={(e) => {
              if (!isTabDrag(e)) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              if (dropTarget !== tab.id) setDropTarget(tab.id);
            }}
            onDragLeave={(e) => {
              if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
              setDropTarget((cur) => (cur === tab.id ? null : cur));
            }}
            onDrop={(e) => {
              if (!isTabDrag(e)) return;
              e.preventDefault();
              const sourceId = e.dataTransfer.getData(TAB_DND_MIME);
              setDropTarget(null);
              setDragging(null);
              if (sourceId && sourceId !== tab.id) moveTab(sourceId, tab.id);
            }}
            onMouseDown={() => setActiveTab(tab.id)}
            onDoubleClick={() => startRename(tab.id, tab.name)}
            className={[
              "group flex shrink-0 cursor-pointer select-none items-center gap-1.5 rounded px-2.5",
              active
                ? "bg-neutral-800 text-neutral-100"
                : "text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200",
              dragging === tab.id ? "opacity-40" : "",
              isDropTarget ? "ring-1 ring-emerald-400" : "",
            ].join(" ")}
            title={tab.name}
          >
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                active ? "bg-emerald-500" : "bg-neutral-600"
              }`}
            />
            {editing === tab.id ? (
              <input
                autoFocus
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onBlur={commitRename}
                onMouseDown={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitRename();
                  else if (e.key === "Escape") setEditing(null);
                }}
                className="w-24 bg-neutral-700 px-1 text-neutral-100 outline-none ring-1 ring-emerald-600"
              />
            ) : (
              <span className="max-w-[12rem] truncate">{tab.name}</span>
            )}
            {tab.order.length > 0 && (
              <span className="text-[10px] text-neutral-500">
                {tab.order.length}
              </span>
            )}
            {closable && (
              <button
                type="button"
                draggable={false}
                onMouseDown={(e) => e.stopPropagation()}
                onDragStart={(e) => e.preventDefault()}
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                className="ml-0.5 hidden shrink-0 rounded px-0.5 leading-none text-neutral-500 hover:bg-neutral-600 hover:text-neutral-100 group-hover:inline"
                title="Close empty tab"
                aria-label={`Close ${tab.name}`}
              >
                ×
              </button>
            )}
          </div>
        );
      })}

      {/* New-tab button, immediately to the right of the right-most tab. */}
      <button
        type="button"
        onClick={() => addTab()}
        className="my-1 shrink-0 self-center rounded px-2 py-0.5 leading-none text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
        title="New workspace tab"
        aria-label="New workspace tab"
      >
        ＋
      </button>
    </div>
  );
}
