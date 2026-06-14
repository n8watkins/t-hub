import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { Canvas } from "./components/Canvas";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";

// 0.5 shell: a persistent Chrome-style top bar, then the body row (the read-only
// supervision sidebar + the terminal canvas). The sidebar is collapsible
// (Ctrl/Cmd+B, handled in Canvas) so the canvas can still go full-width like the
// 0.1 nucleus, and its width is user-resizable (#2). Selecting a session
// surfaces it for now; tab/tile focus routing lands with workspace tabs.
//
// The OS window is frameless (decorations:false); <Titlebar/> is the only window
// chrome. It is a real layout row above the body row; the body row takes the
// remaining height.

// Sidebar width (px) — resizable (#2), persisted to localStorage and clamped to
// a sane range so it can be neither dragged uselessly narrow nor allowed to hog
// the canvas.
const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 480;
const SIDEBAR_DEFAULT = 256; // matches the old fixed w-64 (16rem)
const SIDEBAR_KEY = "termhub.sidebar.v1";

function clampSidebar(n: number): number {
  if (!Number.isFinite(n)) return SIDEBAR_DEFAULT;
  return Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, Math.round(n)));
}
function loadSidebarWidth(): number {
  if (typeof localStorage === "undefined") return SIDEBAR_DEFAULT;
  const raw = Number(localStorage.getItem(SIDEBAR_KEY));
  return raw ? clampSidebar(raw) : SIDEBAR_DEFAULT;
}

export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [, setSelectedSession] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);

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
    <div className="flex h-full w-full flex-col bg-neutral-950 text-neutral-100">
      <Titlebar />
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
