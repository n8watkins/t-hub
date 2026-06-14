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
import { useLayoutEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, Ref, RefObject } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useWorkspace } from "../store/workspace";
import { useSettings } from "../store/settings";
import { startPointerDrag } from "../lib/pointerDrag";
import { createDragGhost, type DragGhost } from "../lib/dragGhost";
import { popOutTab, closeSatellite, readSatelliteTab } from "../lib/windows";
import { closeTerminal } from "../ipc/client";

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

/** Height (px) of the titlebar row (matches the bar's h-8); a drop below this is
 *  out in the canvas, used to decide a tear-off vs an in-strip drop (TASK 2). */
const TITLEBAR_H = 32;

/**
 * True when a drag was released AWAY from the tab strip — out in the canvas area
 * rather than within the titlebar row (TASK 2). The caller only consults this
 * once it knows the release wasn't over any tab; here we just check the release
 * is below the titlebar, so a drop into the strip's own empty/drag region (still
 * within the bar) is NOT treated as a tear-off.
 */
function droppedOutsideStrip(y: number): boolean {
  return y > TITLEBAR_H;
}

/**
 * The top bar. In the MAIN window it hosts the workspace tab strip + new-tab
 * button. In a SATELLITE window (#21, a popped-out tab) there is no strip — just
 * the brand, the popped tab's name, and a "return to main window" control — since
 * a satellite renders exactly one tab and creating/closing tabs there is
 * meaningless.
 */
export function Titlebar({
  satellite = false,
  tabStripOffset = 0,
}: {
  satellite?: boolean;
  /**
   * How far (px) from the window's left edge the workspace tab strip should
   * begin — set by App to the sidebar's current effective width so the leftmost
   * tab aligns with the canvas's left edge (TASK 1). The brand stays pinned at
   * the far left; a draggable spacer between the brand and the strip widens to
   * fill the gap. Updates live as the sidebar mode/width changes. Ignored in a
   * satellite (no tab strip there). Defaults to 0 (brand-hugging strip).
   */
  tabStripOffset?: number;
}) {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  // Measure the brand so the tab-strip spacer can offset by the sidebar width
  // minus the brand's own footprint (TASK 1). Unused in a satellite (no strip).
  const brandRef = useRef<HTMLDivElement | null>(null);
  const brandWidth = useMeasuredWidth(brandRef);
  return (
    <div
      className="flex h-8 shrink-0 items-stretch border-b text-xs"
      style={{
        backgroundColor: "var(--th-titlebar-bg)",
        borderColor: "var(--th-border)",
      }}
    >
      {/* Brand, top-left (#6). Doubles as a left drag handle. */}
      <Brand innerRef={brandRef} />

      {satellite ? (
        // Satellite: show the popped-out tab's name + a return control, then a
        // draggable stretch. No tab strip / new-tab button.
        <SatelliteBar />
      ) : (
        // Main: a draggable spacer that pushes the tab strip out to the sidebar's
        // right edge (TASK 1), then the workspace tabs (+ the new-tab button at
        // the right of the strip), then a flexible drag region.
        <>
          <TabStripSpacer offset={tabStripOffset} brandWidth={brandWidth} />
          <TabStrip />
          <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />
        </>
      )}

      {/* Settings (#3): opens the settings/theme surface (also Ctrl/Cmd+,). */}
      <SettingsButton onClick={toggleSettings} />

      {/* Window controls (top-right). No drag-region, or clicks would drag. In a
          satellite (#6) the redundant close (×) is dropped — the SatelliteBar's
          "Return" button is the single affordance that hands the tab back and
          destroys the window — so only minimize/maximize show there. */}
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

/**
 * Draggable filler between the brand and the tab strip (TASK 1). It widens so
 * the strip begins `offset` px from the window's left edge — i.e. at the
 * sidebar's right / the canvas's left edge — making the leftmost tab align with
 * the canvas. The brand precedes it, so the spacer takes the offset minus the
 * brand's measured width; when the sidebar is hidden (offset 0) or narrower than
 * the brand, the spacer collapses to 0 and the strip simply hugs the brand. The
 * brand width is measured live (a ResizeObserver on the real brand box, via the
 * forwarded ref) so this stays correct across theme/font changes, and the offset
 * itself updates live from App. Carries data-tauri-drag-region so grabbing this
 * gap still moves the window like the rest of the empty bar.
 */
function TabStripSpacer({ offset, brandWidth }: { offset: number; brandWidth: number }) {
  const width = Math.max(0, offset - brandWidth);
  return (
    <div
      data-tauri-drag-region
      aria-hidden
      className="shrink-0"
      style={{ width }}
    />
  );
}

/**
 * Live pixel width of an element via ResizeObserver. Used to measure the brand
 * box so TabStripSpacer can subtract it from the sidebar offset without hard-
 * coding a magic number that would drift with the wordmark/theme/font.
 */
function useMeasuredWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setWidth(el.getBoundingClientRect().width);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return width;
}

/** "T-Hub" wordmark with a small accent glyph, anchored top-left (#6). */
function Brand({ innerRef }: { innerRef?: Ref<HTMLDivElement> }) {
  return (
    <div
      ref={innerRef}
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
// Window controls: minimize, maximize/restore, and (main window only) close.
// Fixed-width hover targets matching the bar height; close goes red on hover.
//
// In a SATELLITE window (#6) the close (×) is intentionally omitted: it would be
// a second control duplicating the SatelliteBar's "Return to main window" button
// (both call closeSatellite). The satellite keeps only minimize/maximize here;
// "Return" is the single affordance that hands the tab back and destroys the
// window.
// ---------------------------------------------------------------------------
function WindowControls({ satellite = false }: { satellite?: boolean }) {
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
      {/* Close (×) — main window only. A satellite returns via "Return" (#6). */}
      {!satellite && (
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
      )}
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

  // Tracks the previous pointerdown for manual double-click detection. A tab is
  // renamed when two quick clicks land on the SAME tab that was ALREADY the
  // active tab when this pair started — i.e. double-clicking the currently-active
  // tab renames it, while double-clicking an inactive tab only activates it (the
  // first click makes it active, the pair doesn't qualify because it wasn't
  // active when the pair began). Prevents accidental renames on tab switches.
  const clickRef = useRef<{ id: string; time: number; wasActive: boolean } | null>(null);

  const startRename = (id: string, name: string) => {
    setEditing(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (editing) renameTab(editing, draft);
    setEditing(null);
  };

  // Close a workspace tab (#5). An EMPTY tab closes immediately. A NON-EMPTY tab
  // is closed behind a confirm; on confirm we DETACH (not kill) each of its
  // terminals via closeTerminal — tmux survives, so the work is reachable again
  // by relaunching/respawning — then drop the tab. closeTab returns the removed
  // tile ids and also guards the last-tab case (returns [] without closing).
  const requestCloseTab = (id: string) => {
    const tab = tabs.find((t) => t.id === id);
    if (!tab) return;
    if (tab.order.length > 0) {
      const n = tab.order.length;
      const ok = window.confirm(
        `Close "${tab.name}"? Its ${n} terminal${n === 1 ? "" : "s"} will be ` +
          `detached (the tmux session${n === 1 ? "" : "s"} keep running and ` +
          `can be reattached later).`,
      );
      if (!ok) return;
    }
    const removed = closeTab(id);
    for (const tid of removed) void closeTerminal(tid).catch(() => {});
  };

  // Pointer-based reorder: pressing a tab activates it; dragging past the
  // threshold reorders it onto whichever tab is released over (moveTab).
  const onTabPointerDown = (tabId: string, e: ReactPointerEvent) => {
    if (editing === tabId) return; // let the rename input own the pointer
    if (e.button !== 0) return;
    const name = tabs.find((t) => t.id === tabId)?.name ?? "Workspace";
    const wasActive = tabId === activeTabId;
    const now = Date.now();
    // Second click of a quick pair on the SAME tab => inline rename, but ONLY if
    // that tab was already active when the FIRST click of the pair landed (the
    // `wasActive` flag recorded then). So double-clicking an INACTIVE tab just
    // activates it (the first click had wasActive=false), while double-clicking
    // the CURRENTLY-ACTIVE tab renames it.
    if (
      clickRef.current &&
      clickRef.current.id === tabId &&
      now - clickRef.current.time < 400 &&
      clickRef.current.wasActive
    ) {
      startRename(tabId, name);
      clickRef.current = null;
      return;
    }
    clickRef.current = { id: tabId, time: now, wasActive };
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
        if (!committed) return;
        if (targetId && targetId !== tabId) {
          // Released over another tab -> reorder within the strip (as before).
          moveTab(tabId, targetId);
        } else if (!targetId && droppedOutsideStrip(y)) {
          // Released AWAY from the strip — not over any tab and below the ~32px
          // titlebar, i.e. out in the canvas — so tear the tab off into its own
          // window (TASK 2), same path as the per-tab pop-out button.
          void popOutTab(tabId);
        }
        // Released over the strip's empty area (no tab, still in the titlebar):
        // no-op, matching the prior behavior.
      },
    });
  };

  return (
    // The strip scrolls horizontally if there are many tabs; it never grows past
    // the available width, so the flexible drag region + controls stay reachable.
    // `overflow-y-hidden` clips the scrollbar gutter so it can't steal the row.
    // `th-scroll-thin` gives that horizontal bar a thin, on-brand look (#4).
    // pl-1: the strip box starts at the sidebar's right edge (via the spacer); a
    // 4px hair of inset keeps the rounded leftmost tab off the seam while still
    // aligning it with the canvas's left edge (TASK 1).
    <div className="th-scroll-thin flex min-w-0 items-stretch gap-1 overflow-x-auto overflow-y-hidden pl-1 pr-1">
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        // Any tab can be closed as long as it isn't the last one. A non-empty tab
        // closes behind a confirm (#5); the close × is always rendered (its space
        // reserved) and only its visibility toggles on hover so the tab never
        // resizes.
        const closable = tabs.length > 1;
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
            // Fixed width (#3): a comfortably wide tab whose size NEVER changes on
            // hover. The pop-out + close buttons always occupy their space (their
            // visibility toggles, not their layout), so revealing them on hover
            // can't shift or resize the tab.
            className={[
              "group flex w-44 shrink-0 cursor-pointer touch-none select-none items-center gap-1.5 rounded px-3",
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
            {/* Pop-out (#21): tear this tab off into its own window. Its space is
                always reserved; only its visibility toggles on hover (#3), so the
                tab never resizes. Available for any tab (the point is to pop out a
                tab WITH terminals); pointerDown is stopped so it never starts a
                tab drag/reorder. When hidden it's also un-clickable (invisible
                drops pointer events) so it can't be hit on a non-hovered tab. */}
            <button
              type="button"
              tabIndex={-1}
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                void popOutTab(tab.id);
              }}
              className="ml-0.5 inline-flex shrink-0 rounded p-0.5 leading-none text-neutral-500 invisible hover:bg-neutral-600 hover:text-neutral-100 group-hover:visible"
              title="Pop out into a new window"
              aria-label={`Pop out ${tab.name} into a new window`}
            >
              <PopOutIcon />
            </button>
            {closable && (
              <button
                type="button"
                tabIndex={-1}
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  requestCloseTab(tab.id);
                }}
                className="ml-0.5 inline-flex shrink-0 rounded px-0.5 leading-none text-neutral-500 invisible hover:bg-neutral-600 hover:text-neutral-100 group-hover:visible"
                title={tab.order.length > 0 ? "Close tab" : "Close empty tab"}
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
