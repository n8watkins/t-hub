import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Canvas } from "./components/Canvas";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { useSettings } from "./store/settings";

// 0.5/0.3 shell: a Chrome-style top bar, then the body row (the read-only
// supervision sidebar + the terminal canvas). The sidebar is collapsible
// (Ctrl/Cmd+B, handled in Canvas) and its width is user-resizable (#2).
//
// The OS window is frameless (decorations:false); <Titlebar/> is the only window
// chrome. When the window is NOT maximized the bar is always visible (a real
// layout row). When MAXIMIZED it auto-hides for max terminal space (#7) and is
// revealed by touching the very top edge; on reveal it either pushes the body
// content down (a layout shift, the default) or overlays it, toggled by the
// `revealPushesContent` setting (#8).

// Sidebar width (px) — resizable (#2), persisted to localStorage and clamped to
// a sane range so it can be neither dragged uselessly narrow nor hog the canvas.
const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 480;
const SIDEBAR_DEFAULT = 256; // matches the old fixed w-64 (16rem)
const SIDEBAR_KEY = "termhub.sidebar.v1";

/** Titlebar height in px (matches <Titlebar/>'s h-8); used for the reveal shift. */
const TITLEBAR_H = 32;
/** Maximized auto-hide delay after the bar is shown / the pointer leaves it. */
const HIDE_AFTER_INITIAL_MS = 2000; // ~2s after maximize (#7)
const HIDE_AFTER_LEAVE_MS = 3000; // ~3s after the pointer leaves the bar (#7)

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
 * Titlebar auto-hide/reveal state for maximized mode (#7/#8). When not
 * maximized the bar is always revealed (no timers). When maximized it reveals
 * briefly then hides after a delay; `reveal()` (top-edge / bar hover) re-shows
 * it and `scheduleHide(ms)` arms the next hide.
 */
function useTitlebarReveal(maximized: boolean): {
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
    if (!maximized) {
      setRevealed(true); // always shown when not maximized
      return;
    }
    // Just maximized: show briefly, then auto-hide.
    setRevealed(true);
    timerRef.current = window.setTimeout(
      () => setRevealed(false),
      HIDE_AFTER_INITIAL_MS,
    );
    return clearTimer;
  }, [maximized, clearTimer]);

  return { revealed, reveal, scheduleHide };
}

export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [, setSelectedSession] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);

  const maximized = useWindowMaximized();
  const revealPushesContent = useSettings((s) => s.revealPushesContent);
  const { revealed, reveal, scheduleHide } = useTitlebarReveal(maximized);

  // The bar is shown whenever not maximized, or when maximized and revealed.
  // In maximized mode it overlays the content unless the push-down setting is on.
  const barShown = !maximized || revealed;
  const overlay = maximized && !revealPushesContent;
  // While maximized, hovering the bar keeps it open; leaving re-arms the hide.
  const barHover = maximized
    ? {
        onPointerEnter: reveal,
        onPointerLeave: () => scheduleHide(HIDE_AFTER_LEAVE_MS),
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

  return (
    <div className="relative flex h-full w-full flex-col bg-neutral-950 text-neutral-100">
      {/* Top-edge reveal hot zone — only while maximized AND the bar is hidden,
          so it never steals clicks from the revealed bar (#7/#8). */}
      {maximized && !barShown && (
        <div
          className="absolute inset-x-0 top-0 z-40 h-1.5"
          onPointerEnter={reveal}
          aria-hidden
        />
      )}

      {/* Titlebar. Overlay mode: absolute, slides over the content. Otherwise an
          in-flow row whose height animates 0<->32px, so revealing it in
          maximized mode shoves the body down (#8). */}
      {overlay ? (
        <div
          className="absolute inset-x-0 top-0 z-30"
          style={{
            transform: barShown ? "translateY(0)" : "translateY(-100%)",
            transition: "transform 140ms ease",
          }}
          onPointerEnter={reveal}
          onPointerLeave={() => scheduleHide(HIDE_AFTER_LEAVE_MS)}
        >
          <Titlebar />
        </div>
      ) : (
        <div
          style={{
            height: barShown ? TITLEBAR_H : 0,
            overflow: "hidden",
            transition: "height 140ms ease",
          }}
          {...barHover}
        >
          <Titlebar />
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {sidebarOpen && (
          <>
            <Sidebar width={sidebarWidth} onSelectSession={setSelectedSession} />
            <SidebarResizer onPointerDown={beginResize} />
          </>
        )}
        <div className="relative min-w-0 flex-1">
          <Canvas onToggleSidebar={() => setSidebarOpen((v) => !v)} />
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
