// PreviewOverlay — a reusable, themed modal surface for "peeking" at content
// (a file, a webpage) without spawning a terminal tile. It is a self-contained
// OVERLAY only: it never touches the canvas/tile/pool model — it floats over the
// whole app like the Settings (ThemeEditor) modal and mirrors that modal's
// chrome 1:1 (a dimmed scrim that closes on outside-click, a centered panel that
// stops click propagation, a header with a title + close control, Esc-to-close,
// and a themed `th-scroll` body).
//
// It is presentational + behavioral only — the host owns the open/closed state
// and passes a `title`, an optional `subtitle` (e.g. a full path under a short
// name), and the body content. The body slot is where callers drop a FilePanel
// reader or a WebPreview. Mounting is the host's job (see FileTree, which mounts
// one instance and toggles it from file clicks / the Web-preview button).

import { type ReactNode, useEffect } from "react";
import { repaintAllTerminals } from "../lib/repaint";

export interface PreviewOverlayProps {
  /** Whether the overlay is shown. When false this renders nothing. */
  open: boolean;
  /** Close request (Esc, the backdrop, or the header's ✕). The host clears its
   *  own open state in response. */
  onClose: () => void;
  /** Primary header label (e.g. a file's basename, or "Web preview"). */
  title: ReactNode;
  /** Optional secondary header line (e.g. the full absolute path or URL). */
  subtitle?: ReactNode;
  /** Optional extra controls rendered in the header, left of the close button
   *  (e.g. an "open externally" button for the web preview). */
  headerExtra?: ReactNode;
  /** The body content (a reader, an iframe surface, …). It fills the panel and
   *  owns its own scrolling. */
  children: ReactNode;
}

/**
 * The reusable preview modal. Matches ThemeEditorPanel's scrim + frame so all
 * "floating" surfaces in the app read the same. The panel is sized generously
 * (within 92vw / 88vh caps) since previews want room; the body region is a flex
 * child so an embedded reader/iframe can fill it and scroll inside `th-scroll`.
 */
export function PreviewOverlay({
  open,
  onClose,
  title,
  subtitle,
  headerExtra,
  children,
}: PreviewOverlayProps) {
  // Esc closes, matching the Settings modal. Only bound while open so we never
  // swallow Escape globally (terminals/inputs may want it otherwise).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  // Opening/closing this overlay adds/removes a full-screen `fixed` layer over
  // the DOM-rendered terminals, which WebView2 can leave on a stale/blank frame.
  // Repaint every terminal on each toggle so the grid never stays muted (same
  // class of bug as the spawn-preset menu). See src/lib/repaint.ts.
  useEffect(() => {
    repaintAllTerminals();
  }, [open]);

  if (!open) return null;

  return (
    // Scrim: a mousedown anywhere outside the panel closes the overlay. The panel
    // stops propagation so inner clicks/drags don't bubble out and close it.
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      onMouseDown={onClose}
      role="dialog"
      aria-modal="true"
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex h-[760px] max-h-[88vh] w-[1000px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        {/* Header: title (+ optional subtitle), any extra controls, then close. */}
        <div
          className="flex shrink-0 items-center justify-between gap-3 border-b px-4 py-2.5"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div className="min-w-0">
            <div
              className="truncate text-sm font-semibold"
              style={{ color: "var(--th-fg)" }}
            >
              {title}
            </div>
            {subtitle != null && (
              <div
                className="truncate text-[11px]"
                style={{ color: "var(--th-fg-muted)" }}
              >
                {subtitle}
              </div>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {headerExtra}
            <button
              type="button"
              onClick={onClose}
              className="-mr-1 flex h-8 w-8 items-center justify-center rounded transition-colors hover:bg-neutral-700/40"
              title="Close (Esc)"
              aria-label="Close preview"
              style={{ color: "var(--th-fg-muted)" }}
            >
              <CloseIcon />
            </button>
          </div>
        </div>

        {/* Body: fills the panel; the embedded surface owns its own scroll. */}
        <div className="min-h-0 flex-1">{children}</div>
      </div>
    </div>
  );
}

/** The same X glyph the Settings modal uses, for visual consistency. */
function CloseIcon() {
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
      <path d="M18 6 6 18" />
      <path d="m6 6 12 12" />
    </svg>
  );
}
