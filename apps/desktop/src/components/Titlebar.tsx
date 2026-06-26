// Persistent Chrome-style top bar for the frameless (decorations:false) main
// window — the primary window chrome.
//
// Main layout, left -> right:
//   [brand (tray icon) + sidebar toggle] · [flexible drag region] · [settings
//   gear] · [window controls: min / max-restore / close]
//
// There is NO workspace tab strip — the sidebar's Workspaces list is the sole
// workspace switcher (item 1). The brand + the flexible stretch carry
// `data-tauri-drag-region`, so grabbing the empty areas moves the window (like
// Chrome). The bar is always visible (~32px) with a subtle 1px bottom border.
//
// The window controls + the settings gear live at the TOP-RIGHT and are ALWAYS in
// the titlebar regardless of the sidebar's collapse state, so they're never
// unreachable. They use the Tauri window API / settings store and must NOT carry
// data-tauri-drag-region, or a click would start a window drag instead.
import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { exit } from "@tauri-apps/plugin-process";
import { useWorkspace } from "../store/workspace";
import { useSettings } from "../store/settings";
import { closeSatellite, readSatelliteTab } from "../lib/windows";
import { useAppName } from "../lib/appName";
import brandIcon from "../assets/brand.png";

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

/**
 * Live "is the window maximized?" flag (BUG 3 — icon reflects state). Seeds from
 * the current state on mount, then re-reads it on every `tauri://resize` (which
 * Tauri fires on the maximize/restore transition however it was triggered — our
 * button, the native Snap flyout, a double-click on the caption, or a keyboard
 * shortcut). Cleans up the listener on unmount. Errors are swallowed so a missing
 * Tauri context (e.g. a browser dev server) just leaves the flag false.
 */
function useMaximizedState(): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const refresh = () => {
      void win
        .isMaximized()
        .then((m) => {
          if (!cancelled) setMaximized(m);
        })
        .catch(() => {});
    };
    refresh();
    void win
      .onResized(() => refresh())
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  return maximized;
}

/**
 * Close the titlebar's × — hide to tray (default) or quit, per the `closeToTray`
 * setting (item 9). The window is frameless (no native close button), so this is
 * the ONLY close path; no backend cooperation is needed. `close()` routes through
 * tray.rs's CloseRequested handler (which prevents-close + hides); `exit(0)`
 * bypasses that to actually quit (matching the tray menu's Quit). The setting is
 * read at click time via getState() (a one-shot action, no need to subscribe).
 */
function closeWindow(): void {
  if (useSettings.getState().closeToTray) {
    void getCurrentWindow()
      .close()
      .catch(() => {});
  } else {
    void exit(0).catch(() => {});
  }
}

/**
 * Win11 Snap Layouts (#snap). The native flyout only appears when the OS gets an
 * `HTMAXBUTTON` from `WM_NCHITTEST` over the *visible* maximize button. The
 * frameless window has no native frame, so `src-tauri/src/win_snap.rs` synthesizes
 * that hit-test — but it needs to know WHERE the button is. Rather than hard-code
 * pixel offsets (fragile: wrong on real Win11 DPI, and silently stale if the
 * top-right controls are rearranged), this hook reports the button's REAL rect to
 * the backend.
 *
 * Contract with `set_maximize_button_rect` (win_snap.rs): we send
 * `{ x, y, width, height }` in **physical pixels relative to the window's
 * top-left**. `getBoundingClientRect()` is CSS px relative to the webview client
 * top-left; for this frameless window the client area == the window (tao answers
 * WM_NCCALCSIZE with 0), so client-relative == window-relative. Multiplying by the
 * window scale factor yields physical px in exactly the space the backend's
 * `WM_NCHITTEST` handler works in (it subtracts GetWindowRect().left/top from the
 * screen point). No screen-position math here, so multi-monitor offsets can't bite.
 *
 * We recompute whenever the rect can move or rescale: on mount, on every resize
 * (the flexible drag region grows/shrinks, shifting the controls), on DPI/scale
 * change, on window move (the monitor's scale may differ), and whenever the
 * maximized flag flips (the restore<->maximize glyph swaps but the slot is the
 * same width; recomputing is cheap insurance against any layout shift). A failed
 * invoke is swallowed — outside Tauri (plain `pnpm dev`) there's no command, and a
 * missed report just means no flyout until the next event fires.
 *
 * `maximized` is passed in (not read here) so a single `useMaximizedState()` in
 * the parent drives BOTH the glyph and the recompute, and the effect re-runs when
 * it changes.
 */
function useReportMaxButtonRect(
  ref: RefObject<HTMLButtonElement | null>,
  maximized: boolean,
): void {
  useEffect(() => {
    let win: ReturnType<typeof getCurrentWindow>;
    try {
      win = getCurrentWindow();
    } catch {
      return; // Not in a Tauri window (e.g. browser dev server): nothing to report.
    }
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    const report = () => {
      const el = ref.current;
      if (!el) return;
      void win
        .scaleFactor()
        .then((scale) => {
          if (cancelled) return;
          const r = el.getBoundingClientRect();
          // getBoundingClientRect is already relative to the webview client
          // top-left == the window top-left for this frameless window. Scale CSS
          // px -> physical px. The backend treats this as window-relative.
          return invoke("set_maximize_button_rect", {
            rect: {
              x: r.left * scale,
              y: r.top * scale,
              width: r.width * scale,
              height: r.height * scale,
            },
          });
        })
        .catch(() => {});
    };

    // Initial report — defer one frame so layout (and the live tab strip) has
    // settled, then send. requestAnimationFrame guards against a 0-size rect read
    // during the first paint.
    const raf = requestAnimationFrame(report);

    const subscribe = (
      register: (cb: () => void) => Promise<() => void>,
    ) => {
      void register(() => report())
        .then((fn) => {
          if (cancelled) fn();
          else unlisteners.push(fn);
        })
        .catch(() => {});
    };
    // Resize: the flexible drag region between the tab strip and the controls
    // grows/shrinks, moving the button. Scale change: DPI moved (physical px shift
    // even at the same CSS position). Move: dragging to a monitor with a different
    // scale changes the physical mapping.
    subscribe((cb) => win.onResized(cb));
    subscribe((cb) => win.onScaleChanged(cb));
    subscribe((cb) => win.onMoved(cb));

    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      for (const fn of unlisteners) fn();
    };
    // Re-run on maximize/restore: the control glyph swaps and a recompute keeps
    // the backend slot exact if anything in the row shifted.
  }, [ref, maximized]);
}

/**
 * The top bar. In the MAIN window it's the brand (tray icon) + sidebar toggle on
 * the left, a drag region, then settings + window controls on the right — no tab
 * strip (item 1: the sidebar Workspaces list is the sole workspace switcher). In a
 * SATELLITE window (#21, a popped-out tab) it's the brand, the popped tab's name,
 * and a "return to main window" control.
 */
export function Titlebar({
  satellite = false,
  onToggleSidebar,
}: {
  satellite?: boolean;
  /** Cycle the sidebar collapse state (full -> rail -> hidden). The collapse
   *  button now lives here in the titlebar's left chrome cluster, so it stays
   *  reachable even when the sidebar is fully hidden. */
  onToggleSidebar?: () => void;
}) {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  return (
    <div
      className="flex h-8 shrink-0 items-stretch border-b text-xs"
      style={{
        backgroundColor: "var(--th-titlebar-bg)",
        borderColor: "var(--th-border)",
      }}
    >
      {satellite ? (
        // Satellite (no sidebar / one tab): it keeps its OWN chrome — the brand,
        // the popped-out tab's name + a return control, and its window controls
        // (minimize/maximize; "Return" replaces close).
        <>
          <Brand />
          <SatelliteBar />
          <WindowControls satellite />
        </>
      ) : (
        // Main (item 1: no tab strip — the sidebar Workspaces list is the sole
        // workspace switcher now). LEFT chrome = brand (tray icon) + sidebar
        // toggle; the rest of the bar is a drag region; settings + window controls
        // sit at the top-right (item 9: settings moved right, near minimize).
        <>
          <LeftChrome onToggleSidebar={onToggleSidebar} />
          <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />
          <WindowControls onSettings={toggleSettings} />
        </>
      )}
    </div>
  );
}

/**
 * The titlebar's LEFT chrome cluster: the T-Hub brand (a window-drag handle),
 * the sidebar collapse toggle, and the settings gear. This replaces the old
 * empty drag spacer + the sidebar's own header row, so the dead space left of the
 * first workspace tab is now useful and the sidebar reclaims that vertical space.
 * The collapse button stays here even when the sidebar is hidden, so it's always
 * reachable. Buttons must NOT carry data-tauri-drag-region (clicks would drag).
 */
function LeftChrome({
  onToggleSidebar,
}: {
  onToggleSidebar?: () => void;
}) {
  return (
    <div className="flex shrink-0 items-stretch">
      <Brand />
      {onToggleSidebar && (
        <button
          type="button"
          aria-label="Toggle sidebar"
          title="Toggle sidebar (Ctrl/Cmd+B)"
          onClick={onToggleSidebar}
          className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
        >
          <SidebarToggleIcon />
        </button>
      )}
    </div>
  );
}

/** A simple sidebar/panel glyph for the collapse toggle. */
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
      <path d="M9 4v16" />
    </svg>
  );
}

/**
 * The settings gear — opens the settings/theme surface (also Ctrl/Cmd+,). Now in
 * the left chrome cluster. Must NOT carry data-tauri-drag-region or the click
 * would start a window drag.
 */
function SettingsButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      aria-label="Settings"
      title="Settings (Ctrl/Cmd+,)"
      onClick={onClick}
      className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
      <GearIcon />
    </button>
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

/** App wordmark led by the app/tray icon (item 9: the brand uses the same mark as
 *  the system tray). A window-drag handle. The text is the Tauri `productName` so
 *  the side-by-side dev build reads "T-Hub Dev" here, matching the tray + title. */
function Brand() {
  const appName = useAppName();
  return (
    <div
      data-tauri-drag-region
      className="flex shrink-0 select-none items-center gap-1.5 pl-2.5 pr-2"
    >
      <img
        src={brandIcon}
        alt=""
        aria-hidden
        draggable={false}
        className="h-4 w-4 shrink-0 rounded-[3px]"
      />
      <span
        className="text-xs font-semibold tracking-tight"
        style={{ color: "var(--th-fg)" }}
      >
        {appName}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Window controls: minimize, maximize/restore, and (main window only) close.
// Always live at the titlebar's top-right. Fixed-width hover targets matching
// the bar height; close goes red on hover.
//
// In a SATELLITE window (#6) the close (×) is intentionally omitted: it would be
// a second control duplicating the SatelliteBar's "Return to main window" button
// (both call closeSatellite). The satellite keeps only minimize/maximize here;
// "Return" is the single affordance that hands the tab back and destroys the
// window.
// ---------------------------------------------------------------------------
function WindowControls({
  satellite = false,
  onSettings,
}: {
  satellite?: boolean;
  /** Open the settings surface. Rendered as a gear at the LEFT of the window
   *  controls (item 9: settings moved to the top-right). Main window only. */
  onSettings?: () => void;
}) {
  // Track the window's maximized state so the middle button reflects it (BUG 3):
  // a single square when restored ("Maximize"), overlapping squares when
  // maximized ("Restore"). Tauri fires `tauri://resize` (onResized) on every
  // size change, including the maximize/restore transition driven from the
  // native WM_SYSCOMMAND in src-tauri/src/win_snap.rs, so polling isMaximized()
  // there keeps the icon in lockstep no matter HOW the toggle happened (our
  // button, the OS Snap flyout, a double-click, or a keyboard shortcut).
  const maximized = useMaximizedState();
  // Ref + report the maximize button's live rect to the backend so the Win11
  // Snap Layouts flyout triggers exactly over the visible button (#snap). See
  // useReportMaxButtonRect for the full physical-px / window-relative contract.
  const maxBtnRef = useRef<HTMLButtonElement>(null);
  useReportMaxButtonRect(maxBtnRef, maximized);
  return (
    <div className="flex shrink-0 items-stretch">
      {/* Settings gear at the top-right, left of the window controls (item 9). */}
      {!satellite && onSettings && <SettingsButton onClick={onSettings} />}
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
        ref={maxBtnRef}
        type="button"
        aria-label={maximized ? "Restore" : "Maximize"}
        title={maximized ? "Restore" : "Maximize"}
        onClick={toggleMaximize}
        className="flex h-8 w-11 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
      >
        {maximized ? (
          // Restore glyph: two overlapping squares (the front one is the window,
          // the offset one behind hints it can return to a smaller size).
          <svg
            width="10"
            height="10"
            viewBox="0 0 10 10"
            aria-hidden
            className="pointer-events-none"
          >
            <rect
              x="1"
              y="3"
              width="6"
              height="6"
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
            />
            <path
              d="M3 3 V1 H9 V7 H7"
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
            />
          </svg>
        ) : (
          // Maximize glyph: a single square (the whole screen).
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
        )}
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
// Small inline icons. They inherit `currentColor` so they follow the button's
// hover state.
// ---------------------------------------------------------------------------

/** Settings gear (the primary titlebar settings control). */
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
