// Side-effect mount: a shared "the window is being moved/resized" flag, so
// focus-triggered work can SUPPRESS itself during an OS modal move/resize loop
// and run once the interaction settles. (Drag-lag fix, "Option A".)
//
// Why: clicking the title bar of an UNFOCUSED window fires a `focus` transition,
// and a wall of focus handlers (repaint every terminal, gitInfo per tile,
// listTerminals / recent / usage IPC) all pile onto the single WebView2 UI thread
// at the exact moment the OS drag loop (WM_ENTERSIZEMOVE) starts — freezing the
// FIRST drag. A second drag soon after has no focus transition, so it's smooth.
//
// Exposes:
//   - isInteracting(): are we mid drag/resize (or a pointer just went down)?
//   - runWhenIdle(fn): run fn next frame, but if interacting, DEFER it until the
//     interaction settles, so it never competes with the drag.
// Imported once at startup from main.tsx.
import { getCurrentWindow } from "@tauri-apps/api/window";

const SETTLED_EVENT = "th-window-settled";
/** Clear `interacting` this long after the last move/resize/pointer event. During
 *  an active drag `onMoved` fires continuously, so this debounce stays armed; it
 *  releases shortly after the drag does. */
const SETTLE_MS = 250;

let interacting = false;
let clearTimer: ReturnType<typeof setTimeout> | undefined;
let mounted = false;

/** True while the window is being moved/resized — or a pointer just went down on
 *  it (the earliest, pre-`focus` signal that a drag may be starting). */
export function isInteracting(): boolean {
  return interacting;
}

function scheduleSettle(): void {
  if (clearTimer) clearTimeout(clearTimer);
  clearTimer = setTimeout(() => {
    interacting = false;
    if (typeof window !== "undefined") {
      window.dispatchEvent(new CustomEvent(SETTLED_EVENT));
    }
  }, SETTLE_MS);
}

function begin(): void {
  interacting = true;
  scheduleSettle();
}

/**
 * Run `fn` on the next frame — but if the window is interacting (a drag/resize),
 * DEFER it until the interaction settles. Used to wrap focus-triggered refreshes
 * so the first drag isn't starved. The one-frame defer (vs running inline in the
 * focus handler) lets a same-click pointerdown mark the interaction first.
 *
 * Returns a CANCEL function: call it (e.g. on a React effect's cleanup) to drop a
 * still-pending run so the deferred `fn` can't fire on an unmounted component.
 */
export function runWhenIdle(fn: () => void): () => void {
  let cancelled = false;
  let onSettle: (() => void) | undefined;
  const raf = requestAnimationFrame(() => {
    if (cancelled) return;
    if (!isInteracting()) {
      fn();
      return;
    }
    onSettle = (): void => {
      window.removeEventListener(SETTLED_EVENT, onSettle!);
      if (cancelled) return;
      fn();
    };
    window.addEventListener(SETTLED_EVENT, onSettle);
  });
  return () => {
    cancelled = true;
    cancelAnimationFrame(raf);
    if (onSettle) window.removeEventListener(SETTLED_EVENT, onSettle);
  };
}

/** Idempotent app-startup mount. */
export function mountWindowInteraction(): void {
  if (mounted) return;
  mounted = true;
  if (typeof window !== "undefined") {
    // Earliest signal: a pointer going down (a title-bar drag begins this way,
    // and it precedes the deferred frame the focus handlers run on).
    window.addEventListener("pointerdown", begin, true);
    window.addEventListener("pointerup", scheduleSettle, true);
  }
  // Continuous during the OS modal move/resize loop.
  try {
    const w = getCurrentWindow();
    void w.onMoved(() => begin()).catch(() => {});
    void w.onResized(() => begin()).catch(() => {});
  } catch {
    /* not in a Tauri window — nothing to subscribe to */
  }
}

// Self-mount on import.
mountWindowInteraction();
