// The single Escape dispatch point for the app's stacked overlay surfaces.
//
// The captain overlay and per-tile fullscreen each used to arm their OWN
// capture-phase window keydown listener, so when both were active at once,
// WHICH one consumed Esc depended on listener registration order -
// nondeterministic from the user's point of view. Canvas now owns the one
// window listener and routes every Escape here, where the order is explicit:
//
//   1. Shift+Esc while the captain overlay is up: pass a LITERAL Esc through
//      to the ACTIVE captain terminal and keep the overlay open. The captain
//      is a Claude session - Esc is how the general interrupts a running turn
//      and dismisses its dialogs - so a summoned captain must still be able to
//      receive one.
//   2. Esc while the titlebar anchor's captain dropdown is up: dismiss it (the
//      dropdown's open flag lives in the captain store precisely so this
//      single dispatch point can close it - no second listener).
//   3. Esc while the captain overlay is up: dismiss it (restores focus).
//   4. Esc otherwise, while a tile is fullscreen: exit fullscreen.
//
// Kept UI-free so the precedence is unit-testable against the live stores.
import { useCaptain } from "../store/captain";
import { usePanels } from "../store/panels";
import { writeTerminal } from "../ipc/client";

/** The literal ESC control byte the Shift+Esc passthrough writes. */
export const ESC_BYTE = "\u001b";

/**
 * The ARMING predicate for Canvas's window-capture Escape listener: true while
 * ANY surface this dispatch point can consume Esc for is up. Canvas subscribes
 * these three flags and only attaches the listener while this holds - kept as
 * a pure function (mirroring handleOverlayEscape's precedence list) so the
 * arming condition is unit-testable and can never drift to a SUBSET of the
 * surfaces handled below (the bug class: a surface handled here but never
 * armed for is a dead Esc key).
 */
export function overlayEscapeArmed(surfaces: {
  fullscreenId: string | null;
  captainOpen: boolean;
  anchorMenuOpen: boolean;
}): boolean {
  return (
    surfaces.fullscreenId != null ||
    surfaces.captainOpen ||
    surfaces.anchorMenuOpen
  );
}

/**
 * Handle one Escape keydown against the overlay surfaces, in the explicit
 * precedence order above. Returns true when the key was consumed (the caller
 * must preventDefault + stop the event) and false to let it fall through to
 * whatever else wants Escape (xterm, dialogs own their armed listeners).
 */
export function handleOverlayEscape(shiftKey: boolean): boolean {
  const cap = useCaptain.getState();
  if (cap.open && shiftKey) {
    // Passthrough: interrupt the ACTIVE captain, don't dismiss the overlay.
    // comms-plane Phase 1 (PR #55 LOW-5): the design §3.1 lists this in the
    // break-glass bucket ("user-initiated modal dismiss"), but it fires ONLY on a
    // human pressing Shift+Esc, so it is genuine HUMAN-origin input - it stays on
    // `writeTerminal` (the human path), NOT the audited break-glass writers. Noted
    // so the spec/impl classification is reconciled on the record.
    if (cap.activeCaptainId) {
      void writeTerminal(cap.activeCaptainId, ESC_BYTE).catch(() => {});
    }
    return true;
  }
  if (cap.anchorMenuOpen) {
    cap.setAnchorMenu(false);
    return true;
  }
  if (cap.open) {
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
