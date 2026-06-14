// Auto-hiding window titlebar for the frameless (decorations:false) main window.
//
// Rationale (PRD §5.3 — screen real estate): TermHub is a terminal-first command
// center where vertical pixels are precious. A permanently-visible OS titlebar
// would steal a row from the canvas, so the window is frameless and this is the
// ONLY window chrome. To reclaim that real estate, the bar auto-hides: it stays
// translated out of view until the pointer approaches the very top edge, then
// slides into place. A thin (~6px) always-present trigger zone detects that hover
// without otherwise eating pointer events, and while hidden the bar is
// `pointer-events-none` so it never blocks interaction with the app beneath it.
import { useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
function close(): void {
  void getCurrentWindow()
    .close()
    .catch(() => {});
}

export function Titlebar() {
  const [revealed, setRevealed] = useState(false);

  return (
    // Full-width container pinned to the top. onMouseEnter fires when the pointer
    // hits either the trigger zone or the bar; onMouseLeave fires only once the
    // pointer leaves the whole container, so the bar stays open while in use.
    <div
      className="fixed inset-x-0 top-0 z-50"
      onMouseEnter={() => setRevealed(true)}
      onMouseLeave={() => setRevealed(false)}
    >
      {/* Invisible ~6px hover trigger at the very top edge. Kept tiny so it does
          not meaningfully cover the app; it only detects the reveal gesture. */}
      <div className="h-1.5 w-full" aria-hidden />

      {/* The bar itself (~28px). Hidden by default: translated fully up out of
          view, faded out, and non-interactive so it neither overlaps content nor
          eats clicks. Reveals with a ~150ms slide+fade. */}
      <div
        className={`absolute inset-x-0 top-0 flex h-7 items-stretch border-b border-neutral-800 bg-neutral-900/95 backdrop-blur transition duration-150 ${
          revealed
            ? "translate-y-0 opacity-100 pointer-events-auto"
            : "-translate-y-full opacity-0 pointer-events-none"
        }`}
      >
        {/* Drag region: dragging anywhere in this flex-1 area moves the window. The
            title text is pointer-events-none so it never blocks the drag region. */}
        <div
          data-tauri-drag-region
          className="flex flex-1 items-center px-3"
        >
          <span className="select-none pointer-events-none text-xs text-neutral-400">
            TermHub
          </span>
        </div>

        {/* Window controls. These must NOT carry data-tauri-drag-region, or they
            would drag the window instead of registering clicks. */}
        <button
          type="button"
          aria-label="Minimize"
          title="Minimize"
          onClick={minimize}
          className="flex h-7 w-10 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
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
          className="flex h-7 w-10 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
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
          onClick={close}
          className="flex h-7 w-10 items-center justify-center text-neutral-300 transition-colors hover:bg-red-600 hover:text-white"
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
    </div>
  );
}
