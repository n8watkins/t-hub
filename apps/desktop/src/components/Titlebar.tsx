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
import { useEffect, useRef } from "react";
import type { RefObject } from "react";
import { createPortal } from "react-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { exit } from "@tauri-apps/plugin-process";
import { Anchor } from "lucide-react";
import { useWorkspace } from "../store/workspace";
import { useSettings } from "../store/settings";
import { useCaptain } from "../store/captain";
import { CaptainStatusDot, useCaptainDisplayLabel } from "./CaptainOverlay";
import { useWindowMaximized } from "../lib/windowMaximized";
import { closeSatellite, readSatelliteTab } from "../lib/windows";
import { useAppName, useAppVersion } from "../lib/appName";
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
 * (the flexible drag region grows/shrinks, shifting the controls — coalesced to
 * ONE report per animation frame so a resize-drag can't flood the IPC), on
 * DPI/scale change (a monitor cross — we refresh the cached scale factor there),
 * and whenever the maximized flag flips (the restore<->maximize glyph swaps but the
 * slot is the same width; recomputing is cheap insurance against any layout shift).
 *
 * We deliberately DO NOT recompute on window MOVE. The button rect is
 * window-relative, so a plain move never changes it; the only move that shifts the
 * physical mapping — crossing to a different-DPI monitor — already fires
 * onScaleChanged. Subscribing onMoved here ran TWO IPC round-trips (`scaleFactor()`
 * + `set_maximize_button_rect`) plus a forced reflow on every move event, and a
 * Windows window-drag fires move events tens of times/sec while the main thread is
 * parked in the OS modal move loop — so those calls piled up against a blocked
 * thread and froze the window on drag. Dropping onMoved (and caching the scale +
 * rAF-coalescing the report) removes that storm entirely. A failed invoke is
 * swallowed — outside Tauri (plain `pnpm dev`) there's no command, and a missed
 * report just means no flyout until the next event fires.
 *
 * `maximized` is passed in (not read here) so a single `useWindowMaximized()` in
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

    // Cache the window scale factor. It only changes on a DPI/monitor cross
    // (onScaleChanged), so we read it ONCE up front and refresh it there — instead
    // of an async `scaleFactor()` IPC on EVERY report. Removing the per-event IPC is
    // half of what makes the report cheap enough to survive a resize-drag.
    let scale = 1;

    // rAF-coalesced report: a burst of resize events (a resize-drag fires onResized
    // tens of times/sec) collapses to ONE rect report per animation frame. The
    // `getBoundingClientRect()` reflow + the single `set_maximize_button_rect` IPC
    // run at most once per frame, never per raw event.
    let rafId = 0;
    const flush = () => {
      rafId = 0;
      const el = ref.current;
      if (cancelled || !el) return;
      const r = el.getBoundingClientRect();
      // getBoundingClientRect is already relative to the webview client top-left ==
      // the window top-left for this frameless window. Scale CSS px -> physical px;
      // the backend treats this as window-relative.
      void invoke("set_maximize_button_rect", {
        rect: {
          x: r.left * scale,
          y: r.top * scale,
          width: r.width * scale,
          height: r.height * scale,
        },
      }).catch(() => {});
    };
    const report = () => {
      if (rafId) return; // already scheduled this frame
      rafId = requestAnimationFrame(flush);
    };

    // Seed the cached scale, then do the initial report (deferred one frame inside
    // `report()`/rAF so layout has settled and we don't read a 0-size rect).
    void win
      .scaleFactor()
      .then((s) => {
        if (!cancelled) scale = s;
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) report();
      });

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
    // Resize: the flexible drag region between the left chrome and the controls
    // grows/shrinks, so the right-anchored maximize button's window-relative rect
    // moves — re-report (coalesced). NOT onMoved: see the doc comment above — a
    // plain move never changes the window-relative rect, and the per-move IPC storm
    // it caused was the window-drag freeze.
    subscribe((cb) => win.onResized(cb));
    // Scale change (DPI / monitor cross): refresh the cached scale FIRST, then
    // re-report in the new physical-px space. Rare, so the `scaleFactor()` IPC here
    // is fine (unlike per-move/per-resize).
    void win
      .onScaleChanged(() => {
        void win
          .scaleFactor()
          .then((s) => {
            if (!cancelled) scale = s;
          })
          .catch(() => {})
          .finally(() => {
            if (!cancelled) report();
          });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisteners.push(fn);
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      if (rafId) cancelAnimationFrame(rafId);
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
      <CaptainButton />
    </div>
  );
}

/**
 * The captain summon anchor (captain-overlay, captain-list): the always-visible
 * affordance for the captain overlay. With pins it shows a COUNT BADGE and
 * clicking opens a dropdown of every pinned captain (MRU order, status dot +
 * name) - clicking one summons it. Accent-lit while the overlay is up, dimmed
 * when nothing is pinned (clicking then is a no-op - the tooltip explains how
 * to pin from a tile header's right-click menu). The dropdown's open flag
 * lives in the captain store so lib/escOverlays' single Esc dispatch point can
 * dismiss it (never a second Esc listener).
 */
function CaptainButton() {
  const captainIds = useCaptain((s) => s.captainIds);
  const open = useCaptain((s) => s.open);
  const menuOpen = useCaptain((s) => s.anchorMenuOpen);
  const count = captainIds.length;
  // Anchor rect source for the portaled dropdown (see CaptainDropdown).
  const btnRef = useRef<HTMLButtonElement>(null);
  return (
    <div className="flex items-stretch">
      <button
        ref={btnRef}
        type="button"
        aria-label="Captain menu"
        aria-pressed={open}
        aria-expanded={menuOpen}
        title={
          count > 0
            ? "Pinned captains - click to list, Ctrl+B C summons/cycles"
            : "No captain pinned - right-click a tile header → Pin as captain"
        }
        onClick={() => {
          if (count > 0) useCaptain.getState().setAnchorMenu(!menuOpen);
        }}
        className="relative flex h-8 w-9 items-center justify-center transition-colors hover:bg-neutral-700"
        style={{
          color: open
            ? "var(--th-accent)"
            : count > 0
              ? "var(--th-fg)"
              : "var(--th-fg-muted)",
        }}
      >
        <Anchor size={14} className="pointer-events-none" aria-hidden />
        {count > 0 && (
          <span
            aria-label={`${count} pinned captain${count === 1 ? "" : "s"}`}
            className="pointer-events-none absolute right-0.5 top-0.5 flex h-3.5 min-w-3.5 items-center justify-center rounded-full px-0.5 text-[9px] font-semibold leading-none"
            style={{
              backgroundColor: "var(--th-accent)",
              color: "var(--th-tile-bg)",
            }}
          >
            {count}
          </span>
        )}
      </button>
      {menuOpen && (
        <CaptainDropdown captainIds={captainIds} anchorRef={btnRef} />
      )}
    </div>
  );
}

/** The dropdown menu's width in px (Tailwind w-64), for viewport clamping. */
const CAPTAIN_MENU_W = 256;

/**
 * The anchor's dropdown: pinned captains in MRU order; click = summon that
 * one. A click-away backdrop closes it (Esc closes via lib/escOverlays).
 *
 * PORTALED to document.body (like WorkspacesList's color picker): App.tsx's
 * default titlebar wrapper is a height:TITLEBAR_H, overflow:hidden box (the
 * auto-hide height animation needs the clip), so anything rendered inside the
 * titlebar subtree below the 32px row is fully clipped — the menu would open
 * invisibly while its fixed backdrop escaped the clip and swallowed the next
 * click. The menu is anchored under the button via its measured rect; the
 * titlebar never scrolls and the anchor lives in the left chrome cluster, so
 * one render-time measurement stays valid for the menu's lifetime.
 *
 * Stacking: z-[59]/z-[60] matches the app's portaled-dropdown tier
 * (WorkspacesList uses 60/61). The captain overlay panel can't be slotted
 * ABOVE this menu — it paints at z-index 1 inside TerminalPoolLayer's z-0
 * stacking context, so any body-level portal paints over it. That's the
 * normal ordering for a transient, user-invoked menu over a passive panel,
 * and clicking a row summons + closes the menu immediately anyway.
 */
function CaptainDropdown({
  captainIds,
  anchorRef,
}: {
  captainIds: string[];
  anchorRef: RefObject<HTMLButtonElement | null>;
}) {
  const activeCaptainId = useCaptain((s) => s.activeCaptainId);
  const close = () => useCaptain.getState().setAnchorMenu(false);
  // The anchor button is mounted before the menu can open (its click sets the
  // flag), so the rect is always available; the fallbacks only guard jsdom.
  const rect = anchorRef.current?.getBoundingClientRect();
  const top = rect?.bottom ?? 32;
  const left = rect
    ? Math.max(8, Math.min(rect.left, window.innerWidth - CAPTAIN_MENU_W - 8))
    : 8;
  return createPortal(
    <>
      {/* Click-away backdrop - no document listener needed. */}
      <div className="fixed inset-0 z-[59]" onPointerDown={close} aria-hidden />
      <div
        role="menu"
        aria-label="Pinned captains"
        className="fixed z-[60] w-64 overflow-hidden rounded-md border py-1 shadow-2xl"
        style={{
          top,
          left,
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
        }}
      >
        {captainIds.map((id) => (
          <CaptainDropdownRow
            key={id}
            terminalId={id}
            active={id === activeCaptainId}
            onSummon={close}
          />
        ))}
      </div>
    </>,
    document.body,
  );
}

function CaptainDropdownRow({
  terminalId,
  active,
  onSummon,
}: {
  terminalId: string;
  active: boolean;
  onSummon: () => void;
}) {
  const label = useCaptainDisplayLabel(terminalId);
  // Same liveness affordance as the overlay switcher chip: a pin whose tile
  // is gone (tab popped out to a satellite) summons as a store-level no-op,
  // so it must READ unavailable instead of silently doing nothing.
  const hasTile = useWorkspace((s) =>
    s.tabs.some((t) => t.order.includes(terminalId)),
  );
  return (
    <button
      type="button"
      role="menuitem"
      title={
        hasTile
          ? `Summon captain - ${label}`
          : `${label} - tile not available (tab popped out?)`
      }
      onClick={() => {
        onSummon();
        useCaptain.getState().summonCaptain(terminalId);
      }}
      className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-neutral-700/40"
      style={{
        color: "var(--th-fg)",
        fontWeight: active ? 600 : 400,
        opacity: hasTile ? 1 : 0.5,
      }}
    >
      <CaptainStatusDot terminalId={terminalId} size={9} />
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {active && (
        <span
          className="shrink-0 text-[9px] uppercase tracking-wide"
          style={{ color: "var(--th-accent)" }}
        >
          active
        </span>
      )}
    </button>
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
  const version = useAppVersion();
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
      {/* Build stamp — top-left so the running build is identifiable at a glance.
          Dim + tabular so it reads as metadata, not chrome. */}
      {version && (
        <span
          className="shrink-0 text-[10px] font-medium tabular-nums opacity-55"
          style={{ color: "var(--th-fg)" }}
          title={`${appName} v${version}`}
        >
          v{version}
        </span>
      )}
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
  // maximized ("Restore"). Backed by the shared single-subscription
  // useWindowMaximized (lib/windowMaximized), which polls isMaximized() on every
  // `tauri://resize` — including the maximize/restore transition driven from the
  // native WM_SYSCOMMAND in src-tauri/src/win_snap.rs — so the icon stays in
  // lockstep no matter HOW the toggle happened (our button, the OS Snap flyout, a
  // double-click, or a keyboard shortcut), with ONE listener shared app-wide.
  const maximized = useWindowMaximized();
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
