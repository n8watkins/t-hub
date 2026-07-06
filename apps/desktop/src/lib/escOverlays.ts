// The single Escape dispatch point for the app's stacked overlay surfaces.
//
// The captain overlay and per-tile fullscreen each used to arm their OWN
// capture-phase window keydown listener, so when both were active at once,
// WHICH one consumed Esc depended on listener registration order -
// nondeterministic from the user's point of view. Canvas now owns the one
// window listener and routes every Escape here, where the order is explicit:
//
//   1. Shift+Esc while the captain overlay is up: pass a LITERAL Esc through
//      to the captain terminal and keep the overlay open. The captain is a
//      Claude session - Esc is how the general interrupts a running turn and
//      dismisses its dialogs - so a summoned captain must still be able to
//      receive one.
//   2. Esc while the captain overlay is up: dismiss it (restores focus).
//   3. Esc otherwise, while a tile is fullscreen: exit fullscreen.
//
// Kept UI-free so the precedence is unit-testable against the live stores.
import { useCaptain } from "../store/captain";
import { usePanels } from "../store/panels";
import { writeTerminal } from "../ipc/client";

/** The literal ESC control byte the Shift+Esc passthrough writes. */
export const ESC_BYTE = "\u001b";

/**
 * Handle one Escape keydown against the overlay surfaces, in the explicit
 * precedence order above. Returns true when the key was consumed (the caller
 * must preventDefault + stop the event) and false to let it fall through to
 * whatever else wants Escape (xterm, dialogs own their armed listeners).
 */
export function handleOverlayEscape(shiftKey: boolean): boolean {
  const cap = useCaptain.getState();
  if (cap.open) {
    if (shiftKey) {
      // Passthrough: interrupt the captain, don't dismiss the overlay.
      if (cap.captainId) {
        void writeTerminal(cap.captainId, ESC_BYTE).catch(() => {});
      }
      return true;
    }
    cap.closeOverlay();
    return true;
  }
  const panels = usePanels.getState();
  if (panels.fullscreenId != null) {
    panels.setFullscreen(null);
    return true;
  }
  return false;
}
