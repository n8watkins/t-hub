// Side-effect mount: force a terminal repaint after a WINDOW-STATE change.
//
// The xterm CANVAS renderer (Terminal.tsx) is fast, but — like the WebView2
// overlay quirk that `repaint.ts` already handles — its canvas backing can be left
// showing a STALE/frozen frame after a window geometry or visibility change
// (maximize, restore, resize) or after the window is un-minimized/re-focused, with
// nothing dirtying the terminals. The symptom: the terminal looks frozen after
// maximize/minimize until you SCROLL it (which forces an xterm refresh).
//
// `repaintAllTerminals()` was only triggered by overlay toggles (Canvas/Preview).
// Here we ALSO trigger it on window-state changes:
//   - `onResized` (Tauri) fires on maximize / restore / resize, and
//   - the DOM `focus` event fires when a minimized window is restored / re-focused.
// Debounced so a resize-DRAG doesn't storm repaints; the repaint runs once the
// change settles. Imported once at startup from main.tsx (next to statusMount).
import { getCurrentWindow } from "@tauri-apps/api/window";
import { repaintAllTerminals } from "./repaint";

let mounted = false;

/** Trailing-edge settle: a final repaint after the last window-state event, to
 *  catch the resolved geometry once a resize-drag releases. Kept short — the
 *  snappy refocus comes from the LEADING-edge rAF repaint below; this just cleans
 *  up the final frame. */
const SETTLE_MS = 50;

/** Idempotent app-startup mount. */
export function mountWindowRepaint(): void {
  if (mounted) return;
  mounted = true;

  let rafId = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const schedule = (): void => {
    // LEADING edge: repaint on the very next frame so the terminal refocuses
    // snappily the instant a maximize/minimize/restore lands (no ~80ms wait).
    // Coalesced to one repaint per frame, so a continuous resize-drag follows the
    // size each frame without storming.
    if (!rafId) {
      rafId = requestAnimationFrame(() => {
        rafId = 0;
        repaintAllTerminals();
      });
    }
    // TRAILING edge: one more repaint once the burst settles, to lock in the final
    // geometry after a resize-drag releases.
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => repaintAllTerminals(), SETTLE_MS);
  };

  // Restore-from-minimize / re-focus: regains focus but fires no resize event.
  if (typeof window !== "undefined") {
    window.addEventListener("focus", schedule);
  }

  // Maximize / restore / resize. Swallow outside a Tauri window (plain dev/tests).
  try {
    void getCurrentWindow()
      .onResized(() => schedule())
      .catch(() => {});
  } catch {
    /* not in a Tauri window — nothing to subscribe to */
  }
}

// Self-mount on import.
mountWindowRepaint();
