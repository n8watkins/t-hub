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

/** How long to wait after the last window-state event before forcing a repaint —
 *  long enough that a resize-drag collapses to one repaint on release, short enough
 *  that a maximize/restore repaints near-instantly. */
const SETTLE_MS = 80;

/** Idempotent app-startup mount. */
export function mountWindowRepaint(): void {
  if (mounted) return;
  mounted = true;

  let timer: ReturnType<typeof setTimeout> | undefined;
  const schedule = (): void => {
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
