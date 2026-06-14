// Persistent Chrome-style top bar for the frameless (decorations:false) main
// window — the ONLY window chrome (shell v2/v3).
//
// Layout, left -> right:
//   [T-Hub brand] · [workspace tab strip + "＋" new-tab button] · [flexible
//   draggable region] · [settings gear] · [window controls]
//
// The brand + the flexible stretch carry `data-tauri-drag-region`, so grabbing
// the empty areas moves the window (like Chrome). The bar is always visible
// (~32px) with a subtle 1px bottom border and participates in layout.
//
// Drag interactions are POINTER-based (see src/lib/pointerDrag.ts): both tile
// drag (Tile.tsx) and tab reorder here avoid HTML5 DnD, which is unreliable over
// xterm's WebGL canvas in WebView2. Each tab carries `data-tab-id`, which makes
// it a drop target for BOTH reordering a tab and dropping a *tile* onto it (the
// tile's drag resolves tabs via elementFromPoint + closest).
//
// Window controls (minimize / maximize-restore / close) and the settings gear
// use the Tauri window API / the settings store and must NOT carry
// data-tauri-drag-region, or a click would start a window drag instead.
import { useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useWorkspace } from "../store/workspace";
import { useSettings } from "../store/settings";
import { startPointerDrag } from "../lib/pointerDrag";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { popOutTab, closeSatellite, readSatelliteTab } from "../lib/windows";

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

/** The workspace-tab id under a viewport point, or null (drag resolution). */
function tabUnder(x: number, y: number): string | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  return el?.closest<HTMLElement>("[data-tab-id]")?.getAttribute("data-tab-id") ?? null;
}

/**
 * The top bar. In the MAIN window it hosts the workspace tab strip + new-tab
 * button. In a SATELLITE window (#21, a popped-out tab) there is no strip — just
 * the brand, the popped tab's name, and a "return to main window" control — since
 * a satellite renders exactly one tab and creating/closing tabs there is
 * meaningless.
 */
export function Titlebar({ satellite = false }: { satellite?: boolean }) {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  return (
    <div
      className="flex h-8 shrink-0 items-stretch border-b text-xs"
      style={{
        backgroundColor: "var(--th-titlebar-bg)",
        borderColor: "var(--th-border)",
      }}
    >
      {/* Brand, top-left (#6). Doubles as a left drag handle. */}
      <Brand />

      {satellite ? (
        // Satellite: show the popped-out tab's name + a return control, then a
        // draggable stretch. No tab strip / new-tab button.
        <SatelliteBar />
      ) : (
        // Main: workspace tabs (+ the new-tab button at the right of the strip),
        // then a flexible drag region.
        <>
          <TabStrip />
          <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />
        </>
      )}

      {/* Settings (#3): opens the settings/theme surface (also Ctrl/Cmd+,). */}
      <SettingsButton onClick={toggleSettings} />

      {/* Window controls (top-right). No drag-region, or clicks would drag. In a
          satellite the close button returns the tab to the main window (destroy +
          pop-in) instead of the main window's close-to-tray hide. */}
      <WindowControls satellite={satellite} />
    </div>
  );
}

/**
 * The middle region for a satellite window (#21): the popped-out tab's name and
 * a "return to main window" button that closes the satellite (handing the tab
 * back to the main window). The remaining stretch is a window-drag handle.
 */
function SatelliteBar() {
  const tabId = readSatelliteTab();
  // The satellite's store holds exactly its one tab; read its name for the label.
  const name = useWorkspace(
    (s) => s.tabs.find((t) => t.id === tabId)?.name ?? "Workspace",
  );
  return (
    <>
      <div
        data-tauri-drag-region
        className="flex min-w-0 flex-1 select-none items-center gap-2 pl-3 pr-1"
      >
        <span className="truncate text-neutral-300">{name}</span>
        <span
          className="shrink-0 rounded px-1.5 py-px text-[10px] uppercase tracking-wide"
          style={{
            backgroundColor:
              "color-mix(in srgb, var(--th-accent) 18%, transparent)",
            color: "var(--th-accent)",
          }}
          title="This tab is popped out into its own window"
        >
          popped out
        </span>
      </div>
      <button
        type="button"
        onClick={() => void closeSatellite()}
        title="Return this tab to the main window"
        aria-label="Return this tab to the main window"
        className="flex h-8 items-center gap-1 px-2.5 text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        <PopInIcon />
        <span className="text-[11px]">Return</span>
      </button>
    </>
  );
}

/** "T-Hub" wordmark with a small accent glyph, anchored top-left (#6). */
function Brand() {
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

// ---------------------------------------------------------------------------
// Settings gear (#3) — opens the settings surface via the settings store.
// ---------------------------------------------------------------------------
function SettingsButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      aria-label="Settings"
      title="Settings (Ctrl/Cmd+,)"
      onClick={onClick}
      className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
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
    </button>
  );
}

// ---------------------------------------------------------------------------
// Window controls: minimize, maximize/restore, close. Fixed-width hover targets
// matching the bar height; close goes red on hover.
//
// In a SATELLITE window (#21) the close button doesn't use the main window's
// close-to-tray hide — it returns the tab to the main window and force-destroys
// the satellite (closeSatellite), so closing a popped-out window cleanly hands
// its tab back rather than leaving a hidden, still-attached client.
// ---------------------------------------------------------------------------
function WindowControls({ satellite = false }: { satellite?: boolean }) {
  const onClose = satellite ? () => void closeSatellite() : closeWindow;
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
        onClick={onClose}
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
// Workspace tab strip (PRD §5.2), hosted in the top bar. Click to activate,
// double-click to rename inline, × to close an empty tab, and drag a tab
// left/right to reorder it (pointer-based). Each tab also accepts a dropped
// TILE (drag-a-tile-onto-a-tab, #1) via its `data-tab-id`. The "＋" new-tab
// button sits immediately to the right of the right-most tab.
// ---------------------------------------------------------------------------
function TabStrip() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const addTab = useWorkspace((s) => s.addTab);
  const renameTab = useWorkspace((s) => s.renameTab);
  const closeTab = useWorkspace((s) => s.closeTab);
  const moveTab = useWorkspace((s) => s.moveTab);
  const setDraggingTab = useWorkspace((s) => s.setDraggingTab);
  const setDropTab = useWorkspace((s) => s.setDropTab);
  const draggingTabId = useWorkspace((s) => s.draggingTabId);
  const dropTabId = useWorkspace((s) => s.dropTabId);

  // id of the tab currently being renamed inline (null = none).
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  // Tracks the previous pointerdown for manual double-click detection. We rename
  // only when the second click of a pair lands on a tab that was ALREADY active
  // at the FIRST click — so renaming an unfocused tab takes three clicks total
  // (one to focus, then a double-click). Prevents accidental renames.
  const clickRef = useRef<{ id: string; time: number; wasActiveAtFirst: boolean } | null>(null);

  const startRename = (id: string, name: string) => {
    setEditing(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (editing) renameTab(editing, draft);
    setEditing(null);
  };

  // Pointer-based reorder: pressing a tab activates it; dragging past the
  // threshold reorders it onto whichever tab is released over (moveTab).
  const onTabPointerDown = (tabId: string, e: ReactPointerEvent) => {
    if (editing === tabId) return; // let the rename input own the pointer
    if (e.button !== 0) return;
    const name = tabs.find((t) => t.id === tabId)?.name ?? "Workspace";
    const wasActive = tabId === activeTabId;
    const now = Date.now();
    // Second click of a pair on an already-active tab => rename, no drag.
    if (
      clickRef.current &&
      clickRef.current.id === tabId &&
      now - clickRef.current.time < 400 &&
      clickRef.current.wasActiveAtFirst
    ) {
      startRename(tabId, name);
      clickRef.current = null;
      return;
    }
    clickRef.current = { id: tabId, time: now, wasActiveAtFirst: wasActive };
    setActiveTab(tabId);
    let ghost: DragGhost | null = null;
    startPointerDrag(e.clientX, e.clientY, {
      onBegin: () => {
        setDraggingTab(tabId);
        document.body.dataset.thDragging = "1";
        ghost = createDragGhost({ title: name, width: 150 });
      },
      onMove: (x, y) => {
        ghost?.move(x, y);
        const overId = tabUnder(x, y);
        setDropTab(overId && overId !== tabId ? overId : null);
      },
      onEnd: (x, y, committed) => {
        const targetId = committed ? tabUnder(x, y) : null;
        ghost?.destroy();
        ghost = null;
        delete document.body.dataset.thDragging;
        setDraggingTab(null);
        setDropTab(null);
        if (committed && targetId && targetId !== tabId) moveTab(tabId, targetId);
      },
    });
  };

  return (
    // The strip scrolls horizontally if there are many tabs; it never grows past
    // the available width, so the flexible drag region + controls stay reachable.
    // `overflow-y-hidden` clips the scrollbar gutter so it can't steal the row.
    <div className="flex min-w-0 items-stretch gap-1 overflow-x-auto overflow-y-hidden pl-4 pr-1">
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        const closable = tabs.length > 1 && tab.order.length === 0;
        // Highlighted as a drop target by EITHER a tab reorder or a tile being
        // dragged onto it; never highlight the tab being dragged itself.
        const isDropTarget = dropTabId === tab.id && draggingTabId !== tab.id;
        const isDragging = draggingTabId === tab.id;
        return (
          <div
            key={tab.id}
            // data-tab-id: the drop target a tab reorder / a tile drag resolves to.
            data-tab-id={tab.id}
            onPointerDown={(e) => onTabPointerDown(tab.id, e)}
            className={[
              "group flex min-w-[8.5rem] shrink-0 cursor-pointer touch-none select-none items-center gap-1.5 rounded px-3",
              active
                ? "bg-neutral-800 text-neutral-100"
                : "text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200",
              isDragging ? "opacity-40" : "",
            ].join(" ")}
            style={
              isDropTarget
                ? { boxShadow: "0 0 0 1px var(--th-accent)" }
                : undefined
            }
            title={tab.name}
          >
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                active ? "" : "bg-neutral-600"
              }`}
              style={active ? { backgroundColor: "var(--th-accent)" } : undefined}
            />
            {editing === tab.id ? (
              <input
                autoFocus
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onBlur={commitRename}
                onPointerDown={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitRename();
                  else if (e.key === "Escape") setEditing(null);
                }}
                className="min-w-0 flex-1 bg-neutral-700 px-1 text-neutral-100 outline-none"
                style={{ boxShadow: "0 0 0 1px var(--th-accent)" }}
              />
            ) : (
              <span className="min-w-0 flex-1 truncate">{tab.name}</span>
            )}
            {tab.order.length > 0 && (
              <span
                className="shrink-0 rounded-full px-1.5 text-[11px] font-semibold leading-tight"
                style={{
                  backgroundColor: "color-mix(in srgb, var(--th-accent) 22%, transparent)",
                  color: "var(--th-accent)",
                }}
              >
                {tab.order.length}
              </span>
            )}
            {/* Pop-out (#21): tear this tab off into its own window. Shown on
                hover next to the close ×. Available for any tab (the point is to
                pop out a tab WITH terminals); pointerDown is stopped so it never
                starts a tab drag/reorder. */}
            <button
              type="button"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                void popOutTab(tab.id);
              }}
              className="ml-0.5 hidden shrink-0 rounded p-0.5 leading-none text-neutral-500 hover:bg-neutral-600 hover:text-neutral-100 group-hover:inline-flex"
              title="Pop out into a new window"
              aria-label={`Pop out ${tab.name} into a new window`}
            >
              <PopOutIcon />
            </button>
            {closable && (
              <button
                type="button"
                onPointerDown={(e) => e.stopPropagation()}
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

// ---------------------------------------------------------------------------
// Small inline icons for the tear-off controls (#21). Sized to sit inline with
// the tab text; they inherit `currentColor` so they follow the button's hover.
// ---------------------------------------------------------------------------

/** "Open in new window" arrow (pop a tab out). */
function PopOutIcon() {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <path d="M14 4h6v6" />
      <path d="M20 4 11 13" />
      <path d="M19 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h4" />
    </svg>
  );
}

/** Arrow pointing back into a frame (return a popped tab to the main window). */
function PopInIcon() {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <path d="M9 10 4 5" />
      <path d="M4 9V5h4" />
      <path d="M20 4H10a2 2 0 0 0-2 2v4" />
      <path d="M5 14v4a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V8" />
    </svg>
  );
}
