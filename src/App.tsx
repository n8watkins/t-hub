import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Canvas } from "./components/Canvas";
import { Sidebar, SIDEBAR_RAIL_WIDTH, type SidebarMode } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { useSettings } from "./store/settings";
import { initWindowSync, isSatellite } from "./lib/windows";
import { LifecycleKeybinds } from "./lib/useLifecycleKeybinds";

// Multi-window tear-off (#21): a window opened with `?tab=<id>` is a SATELLITE
// rendering only that one tab (the workspace store scopes itself at boot). The
// main window (no `?tab=`) renders everything except popped-out tabs. Captured
// once at module load so it's stable for this window's lifetime.
const SATELLITE = isSatellite();

// 0.5/0.3 shell: a Chrome-style top bar, then the body row (the read-only
// supervision sidebar + the terminal canvas). The sidebar has a 3-state collapse
// (Ctrl/Cmd+B, handled in Canvas) cycling full -> rail -> hidden -> full (#1),
// and in its full state its width is user-resizable (#2).
//
// The OS window is frameless (decorations:false); <Titlebar/> is the only window
// chrome. By default the bar is ALWAYS a visible layout row (so maximize/restore
// is always reachable). Auto-hide-when-maximized is OPT-IN
// (settings.autoHideTitlebarMaximized): when enabled and the window is
// maximized, the bar auto-hides for max terminal space and is revealed by
// touching the very top edge — either pushing content down (default) or
// overlaying it, per settings.revealPushesContent (#7/#8).

// Sidebar width (px) — resizable (#2), persisted to localStorage and clamped to
// a sane range so it can be neither dragged uselessly narrow nor hog the canvas.
const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 360;
const SIDEBAR_DEFAULT = 256; // matches the old fixed w-64 (16rem)
const SIDEBAR_KEY = "termhub.sidebar.v1";

// 3-state collapse (#1): the sidebar cycles full -> rail -> hidden -> full via
// onToggleSidebar (Ctrl/Cmd+B, fired by Canvas). "full" keeps the resizable
// width; "rail" is a thin iconic strip; "hidden" drops it entirely. The chosen
// mode is persisted to its OWN localStorage key (independent of the width key).
const SIDEBAR_MODE_KEY = "termhub.sidebar.mode.v1";
const SIDEBAR_MODES: SidebarMode[] = ["full", "rail", "hidden"];

function loadSidebarMode(): SidebarMode {
  if (typeof localStorage === "undefined") return "full";
  const raw = localStorage.getItem(SIDEBAR_MODE_KEY);
  return raw === "full" || raw === "rail" || raw === "hidden" ? raw : "full";
}
/** Advance full -> rail -> hidden -> full (the Ctrl/Cmd+B cycle). */
function nextSidebarMode(m: SidebarMode): SidebarMode {
  const i = SIDEBAR_MODES.indexOf(m);
  return SIDEBAR_MODES[(i + 1) % SIDEBAR_MODES.length];
}

/** Titlebar height in px (matches <Titlebar/>'s h-8); used for the reveal shift. */
const TITLEBAR_H = 32;
// The maximized auto-hide delay (after the bar is shown / the pointer leaves it)
// and the reveal slide duration are now user-configurable in Settings
// (settings.titlebarHideDelayMs / settings.titlebarRevealAnimMs).

function clampSidebar(n: number): number {
  if (!Number.isFinite(n)) return SIDEBAR_DEFAULT;
  return Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, Math.round(n)));
}
function loadSidebarWidth(): number {
  if (typeof localStorage === "undefined") return SIDEBAR_DEFAULT;
  const raw = Number(localStorage.getItem(SIDEBAR_KEY));
  return raw ? clampSidebar(raw) : SIDEBAR_DEFAULT;
}

/** Track the Tauri window's maximized state, updating on every resize. */
function useWindowMaximized(): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    try {
      const win = getCurrentWindow();
      const check = () => {
        win
          .isMaximized()
          .then((m) => {
            if (!disposed) setMaximized(m);
          })
          .catch(() => {});
      };
      check();
      win
        .onResized(() => check())
        .then((fn) => {
          if (disposed) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // Not running inside a Tauri window (e.g. plain `pnpm dev`): stay false.
    }
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  return maximized;
}

/**
 * Titlebar auto-hide/reveal state. `enabled` is true only when the window is
 * maximized AND the user opted into auto-hide; when false the bar is always
 * revealed (no timers), so the default experience keeps a permanent visible bar.
 * `initialHideMs` is the configurable delay before the bar auto-hides after the
 * initial maximize reveal (settings.titlebarHideDelayMs).
 */
function useTitlebarReveal(
  enabled: boolean,
  initialHideMs: number,
): {
  revealed: boolean;
  reveal: () => void;
  scheduleHide: (ms: number) => void;
} {
  const [revealed, setRevealed] = useState(true);
  const timerRef = useRef<number | undefined>(undefined);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== undefined) {
      window.clearTimeout(timerRef.current);
      timerRef.current = undefined;
    }
  }, []);
  const reveal = useCallback(() => {
    clearTimer();
    setRevealed(true);
  }, [clearTimer]);
  const scheduleHide = useCallback(
    (ms: number) => {
      clearTimer();
      timerRef.current = window.setTimeout(() => setRevealed(false), ms);
    },
    [clearTimer],
  );

  useEffect(() => {
    clearTimer();
    if (!enabled) {
      setRevealed(true); // always shown unless auto-hide is active
      return;
    }
    // Auto-hide active: show briefly, then hide.
    setRevealed(true);
    timerRef.current = window.setTimeout(() => setRevealed(false), initialHideMs);
    return clearTimer;
  }, [enabled, initialHideMs, clearTimer]);

  return { revealed, reveal, scheduleHide };
}

export default function App() {
  // A satellite starts with the supervision sidebar hidden — it's a focused
  // terminal canvas, not the full command center. The main window restores the
  // user's persisted collapse mode (#1: full / rail / hidden).
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>(() =>
    SATELLITE ? "hidden" : loadSidebarMode(),
  );
  const [, setSelectedSession] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);

  // Cycle the collapse state (full -> rail -> hidden -> full) and persist it.
  // This is exactly what App hands to Canvas as onToggleSidebar, so Canvas's
  // unchanged Ctrl/Cmd+B keybinding now advances through all three states.
  const cycleSidebarMode = useCallback(() => {
    setSidebarMode((m) => {
      const next = nextSidebarMode(m);
      try {
        localStorage.setItem(SIDEBAR_MODE_KEY, next);
      } catch {
        /* ignore quota/availability */
      }
      return next;
    });
  }, []);

  // Wire cross-window tear-off resync once for this window (#21): the main window
  // hides/re-adopts tabs as satellites open/close; a satellite self-closes if its
  // tab is reclaimed. Best-effort — a failure just disables live cross-window
  // sync (persistence still keeps a fresh launch consistent).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void initWindowSync()
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error("initWindowSync failed", err));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const maximized = useWindowMaximized();
  const revealPushesContent = useSettings((s) => s.revealPushesContent);
  const autoHide = useSettings((s) => s.autoHideTitlebarMaximized);
  // Configurable auto-hide timings (Settings -> General -> Titlebar).
  const hideDelayMs = useSettings((s) => s.titlebarHideDelayMs);
  const revealAnimMs = useSettings((s) => s.titlebarRevealAnimMs);
  // Auto-hide only kicks in when maximized AND opted in; otherwise the titlebar
  // is always a visible row so maximize/restore is always reachable.
  const hideable = maximized && autoHide;
  const { revealed, reveal, scheduleHide } = useTitlebarReveal(
    hideable,
    hideDelayMs,
  );
  // Reveal/hide slide duration, shared by both bar layout modes.
  const barTransition = `${revealAnimMs}ms ease`;

  // The bar is shown whenever auto-hide isn't active, or when it is and revealed.
  const barShown = !hideable || revealed;
  const overlay = hideable && !revealPushesContent;
  // While auto-hiding, hovering the bar keeps it open; leaving re-arms the hide.
  const barHover = hideable
    ? {
        onPointerEnter: reveal,
        onPointerLeave: () => scheduleHide(hideDelayMs),
      }
    : undefined;

  // --- Sidebar resize (#2) ---
  // Drag the divider between the sidebar and the canvas to set the sidebar
  // width; persist on release. Pointer-based (window listeners) so the drag
  // keeps tracking over the canvas/terminals (same reason as the tile drag).
  const resizeRef = useRef<{ startX: number; startW: number } | null>(null);
  const widthRef = useRef(sidebarWidth);
  widthRef.current = sidebarWidth;

  const onResizeMove = useCallback((e: PointerEvent) => {
    const d = resizeRef.current;
    if (!d) return;
    setSidebarWidth(clampSidebar(d.startW + (e.clientX - d.startX)));
  }, []);
  const onResizeEnd = useCallback(() => {
    if (!resizeRef.current) return;
    resizeRef.current = null;
    window.removeEventListener("pointermove", onResizeMove);
    window.removeEventListener("pointerup", onResizeEnd);
    document.body.style.removeProperty("cursor");
    document.body.style.removeProperty("user-select");
    try {
      localStorage.setItem(SIDEBAR_KEY, String(widthRef.current));
    } catch {
      /* ignore quota/availability */
    }
  }, [onResizeMove]);
  const beginResize = useCallback(
    (e: ReactPointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      resizeRef.current = { startX: e.clientX, startW: widthRef.current };
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      window.addEventListener("pointermove", onResizeMove);
      window.addEventListener("pointerup", onResizeEnd);
    },
    [onResizeMove, onResizeEnd],
  );

  // Detach window listeners if we unmount mid-resize.
  useEffect(() => {
    return () => {
      window.removeEventListener("pointermove", onResizeMove);
      window.removeEventListener("pointerup", onResizeEnd);
    };
  }, [onResizeMove, onResizeEnd]);

  // The sidebar's current effective on-screen width (TASK 1). The titlebar uses
  // this to indent its tab strip so the leftmost tab aligns with the canvas's
  // left edge (the sidebar's right edge): 0 when hidden, the rail width when
  // railed, the resizable width when full. The SidebarResizer itself has a net-
  // zero layout footprint (-mx-[3px] on a w-1.5 box), so the canvas's left edge
  // sits exactly this many px from the window's left. Updates live as the mode/
  // width changes because it's derived straight from the state each render.
  const sidebarOffset =
    sidebarMode === "hidden"
      ? 0
      : sidebarMode === "rail"
        ? SIDEBAR_RAIL_WIDTH
        : sidebarWidth;

  return (
    <div className="relative flex h-full w-full flex-col bg-neutral-950 text-neutral-100">
      {/* Lifecycle keybinds (feat/lifecycle): Ctrl/Cmd+Shift+W deletes the focused
          terminal's session behind a confirm (Ctrl/Cmd+W still detaches). Renders
          only its confirm dialog when armed. */}
      <LifecycleKeybinds />
      {/* Top-edge reveal hot zone — only while auto-hide is active AND the bar is
          hidden, so it never steals clicks from the visible bar (#7/#8). */}
      {hideable && !barShown && (
        <div
          className="absolute inset-x-0 top-0 z-40 h-1.5"
          onPointerEnter={reveal}
          aria-hidden
        />
      )}

      {/* Titlebar. Overlay mode: absolute, slides over the content. Otherwise an
          in-flow row whose height animates 0<->32px, so revealing it in
          auto-hide mode shoves the body down (#8). */}
      {overlay ? (
        <div
          className="absolute inset-x-0 top-0 z-30"
          style={{
            transform: barShown ? "translateY(0)" : "translateY(-100%)",
            transition: `transform ${barTransition}`,
          }}
          onPointerEnter={reveal}
          onPointerLeave={() => scheduleHide(hideDelayMs)}
        >
          <Titlebar satellite={SATELLITE} tabStripOffset={sidebarOffset} />
        </div>
      ) : (
        <div
          style={{
            height: barShown ? TITLEBAR_H : 0,
            overflow: "hidden",
            transition: `height ${barTransition}`,
          }}
          {...barHover}
        >
          <Titlebar satellite={SATELLITE} tabStripOffset={sidebarOffset} />
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {sidebarMode !== "hidden" && (
          <>
            <Sidebar
              mode={sidebarMode}
              width={sidebarMode === "rail" ? SIDEBAR_RAIL_WIDTH : sidebarWidth}
              onSelectSession={setSelectedSession}
              onToggleSidebar={cycleSidebarMode}
            />
            {/* Drag-resize only applies to the full state; the rail is fixed. */}
            {sidebarMode === "full" && (
              <SidebarResizer onPointerDown={beginResize} />
            )}
          </>
        )}
        <div className="relative min-w-0 flex-1">
          <Canvas onToggleSidebar={cycleSidebarMode} />
        </div>
      </div>
    </div>
  );
}

/**
 * The draggable divider on the sidebar's right edge (#2). A wide, invisible
 * hit zone (col-resize cursor) straddling the seam, with a thin centered
 * indicator that picks up the accent on hover (the shared `.th-gutter-line`
 * rule, same as the canvas resize gutters).
 */
function SidebarResizer({
  onPointerDown,
}: {
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={onPointerDown}
      title="Drag to resize the sidebar"
      className="group relative z-10 -mx-[3px] w-1.5 shrink-0 cursor-col-resize touch-none"
    >
      <div className="th-gutter-line absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-neutral-700/60 transition-colors" />
    </div>
  );
}
