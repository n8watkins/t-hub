// Generic pointer-drag controller shared by tile drag (Tile.tsx) and workspace-
// tab drag (Titlebar.tsx).
//
// Why this exists: HTML5 drag-and-drop is unreliable over xterm's WebGL canvas
// in WebView2 — the canvas/hidden-textarea swallow the drag events, so drops
// never fire. So every TermHub drag interaction is built on POINTER events plus
// `document.elementFromPoint`, not native DnD. This module owns the fiddly,
// easy-to-get-wrong parts so both call sites stay small and correct:
//   - a small move-threshold, so a plain click still focuses/activates without
//     ever starting a drag;
//   - window-level (capture-phase) move/up listeners, so the drag keeps tracking
//     even while the pointer is over a terminal;
//   - grabbing cursor + text-selection suppression for the duration;
//   - deterministic cleanup on pointerup / pointercancel / Escape.
//
// The caller resolves the actual drop target itself (via elementFromPoint) inside
// onMove/onEnd — this controller is intentionally target-agnostic.

export interface PointerDragHandlers {
  /** Pixels the pointer must travel before a drag actually begins (default 4). */
  threshold?: number;
  /** Fired once, the moment the threshold is first crossed (drag truly starts). */
  onBegin?: () => void;
  /** Fired on every move after the drag has begun, with viewport coordinates. */
  onMove: (x: number, y: number) => void;
  /**
   * Fired exactly once at the end. `committed` is true only if a drag began
   * (threshold crossed) AND the pointer was released normally — it is false for
   * a plain click (never crossed threshold) or a cancel (pointercancel/Escape).
   */
  onEnd: (x: number, y: number, committed: boolean) => void;
}

/**
 * Begin tracking a potential drag from a pointerdown at (`startX`, `startY`).
 * Returns immediately; tracking continues via window listeners until the pointer
 * is released, the gesture is cancelled, or Escape is pressed.
 */
export function startPointerDrag(
  startX: number,
  startY: number,
  handlers: PointerDragHandlers,
): void {
  const threshold = handlers.threshold ?? 4;
  let begun = false;
  let cancelled = false;

  const cleanup = (): void => {
    window.removeEventListener("pointermove", onMove, true);
    window.removeEventListener("pointerup", onUp, true);
    window.removeEventListener("pointercancel", onCancel, true);
    window.removeEventListener("keydown", onKey, true);
    if (begun) {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
    }
  };

  const onMove = (e: PointerEvent): void => {
    if (!begun) {
      if (
        Math.abs(e.clientX - startX) < threshold &&
        Math.abs(e.clientY - startY) < threshold
      ) {
        return; // still within the click slop — not a drag yet
      }
      begun = true;
      document.body.style.cursor = "grabbing";
      document.body.style.userSelect = "none";
      handlers.onBegin?.();
    }
    handlers.onMove(e.clientX, e.clientY);
  };

  const finish = (x: number, y: number): void => {
    const committed = begun && !cancelled;
    cleanup();
    handlers.onEnd(x, y, committed);
  };

  const onUp = (e: PointerEvent): void => finish(e.clientX, e.clientY);
  const onCancel = (e: PointerEvent): void => {
    cancelled = true;
    finish(e.clientX, e.clientY);
  };
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      cancelled = true;
      finish(startX, startY);
    }
  };

  window.addEventListener("pointermove", onMove, true);
  window.addEventListener("pointerup", onUp, true);
  window.addEventListener("pointercancel", onCancel, true);
  window.addEventListener("keydown", onKey, true);
}
