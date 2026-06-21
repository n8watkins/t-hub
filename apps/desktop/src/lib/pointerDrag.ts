// Generic pointer-drag controller shared by tile drag (Tile.tsx) and workspace-
// tab drag (Titlebar.tsx).
//
// Why this exists: HTML5 drag-and-drop is unreliable over xterm's WebGL canvas
// in WebView2 — the canvas/hidden-textarea swallow the drag events, so drops
// never fire. So every T-Hub drag interaction is built on POINTER events plus
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
  /**
   * When true, this controller OWNS the global `document.body.dataset.thDragging`
   * flag (#8): it sets it on begin and clears it in cleanup (the same place the
   * grabbing cursor is cleared), so call sites no longer hand-manage it and a
   * cancelled/unmounted drag can never leak a stuck flag that leaves every
   * terminal pointer-inert (index.css gates `[data-th-pool-tile]`/`.xterm` on
   * it). Defaults to false to preserve the old behavior for any unported caller.
   */
  manageBodyDragFlag?: boolean;
}

/**
 * Cancel an in-flight pointer drag (#3). Calling it runs the SAME cleanup as a
 * pointercancel — removes the window listeners, clears the grabbing cursor (and
 * the body drag flag when this controller owns it), and fires `onEnd` with
 * `committed=false` — so an unmount mid-drag can't leak listeners or a stuck
 * state. Idempotent: a no-op after the drag has already finished or been
 * cancelled.
 */
export type PointerDragCanceller = () => void;

/**
 * Begin tracking a potential drag from a pointerdown at (`startX`, `startY`).
 * Returns a CANCELLER (#3): call it from a component's unmount cleanup to abort an
 * in-flight drag (cleans up listeners + the grabbing cursor + the owned body flag
 * and fires onEnd with committed=false). Tracking otherwise continues via window
 * listeners until the pointer is released, the gesture is cancelled, or Escape is
 * pressed; the normal complete/cancel paths are unchanged.
 */
export function startPointerDrag(
  startX: number,
  startY: number,
  handlers: PointerDragHandlers,
): PointerDragCanceller {
  const threshold = handlers.threshold ?? 4;
  let begun = false;
  let cancelled = false;
  let finished = false;

  const cleanup = (): void => {
    window.removeEventListener("pointermove", onMove, true);
    window.removeEventListener("pointerup", onUp, true);
    window.removeEventListener("pointercancel", onCancel, true);
    window.removeEventListener("keydown", onKey, true);
    if (begun) {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      // Clear the owned body drag flag alongside the cursor, so terminals stop
      // being pointer-inert the instant the gesture ends however it ended.
      if (handlers.manageBodyDragFlag) delete document.body.dataset.thDragging;
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
      // Set the body flag (when owned) the moment the drag truly begins, matching
      // where call sites used to set it in onBegin.
      if (handlers.manageBodyDragFlag) document.body.dataset.thDragging = "1";
      handlers.onBegin?.();
    }
    handlers.onMove(e.clientX, e.clientY);
  };

  const finish = (x: number, y: number): void => {
    if (finished) return; // already ended/cancelled — stay idempotent
    finished = true;
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

  // The canceller: abort exactly like a pointercancel (committed=false) from the
  // drag's start point. Idempotent via `finished`, so calling it after a normal
  // end is harmless.
  return (): void => {
    if (finished) return;
    cancelled = true;
    finish(startX, startY);
  };
}
