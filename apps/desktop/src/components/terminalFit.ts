// Degenerate-measurement guard for the terminal resize path.
//
// A terminal tile that is parked offscreen, display:none, or simply not laid out
// yet measures ~0px wide. xterm's FitAddon then floors its proposal at
// MINIMUM_COLS (2) / MINIMUM_ROWS (1), and pushing that geometry to the PTY
// (TIOCSWINSZ) shrinks the WHOLE tmux window to ~2 columns — the wedged "2x24
// client" bug. That forced captains to work around it with
// `window-size manual 220x50`, which then overshoots the real tile (~124-147
// cols) and clips content off the right edge (and paints tmux's fill-character
// dot field in the unused area).
//
// This lives in its own module (no xterm runtime import — the term/fit types are
// type-only, erased at build) so the pure guard can be unit-tested without
// dragging the full Terminal.tsx / xterm canvas import chain into jsdom.

import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";

// A sane floor for the geometry we push to the PTY. No real terminal tile is this
// small, so a proposal below this floor is always a measurement artifact, never a
// genuine size. See tmux.rs::reassert_window_size_latest for the backend
// belt-and-braces half of the same fix.
export const MIN_SANE_COLS = 20;
export const MIN_SANE_ROWS = 5;

/**
 * The FitAddon's proposed geometry, but ONLY when the tile is genuinely laid out
 * at a readable size. Returns `null` when the tile is unmeasurable (no element
 * yet, zero-width box) or the proposal is degenerate/sub-floor — the caller must
 * then NOT fit or resize, leaving the terminal at its current (sane) geometry
 * until it gets a real box (the ResizeObserver / pool-move re-fits it then). This
 * is the single guard that keeps a background/pre-layout tile from ever reporting
 * ~2 cols to the PTY.
 */
export function saneFitProposal(
  term: Terminal,
  fit: FitAddon,
): { cols: number; rows: number } | null {
  const parent = term.element?.parentElement;
  // No element yet, or a zero-width box: parked offscreen / display:none /
  // pre-layout. Bail before FitAddon floors the measurement to 2 cols.
  if (!parent || parent.offsetWidth < 1 || parent.clientWidth < 1) return null;
  let proposed: { cols: number; rows: number } | undefined;
  try {
    proposed = fit.proposeDimensions();
  } catch {
    return null;
  }
  if (
    !proposed ||
    !Number.isFinite(proposed.cols) ||
    !Number.isFinite(proposed.rows) ||
    proposed.cols < MIN_SANE_COLS ||
    proposed.rows < MIN_SANE_ROWS
  ) {
    return null;
  }
  return { cols: proposed.cols, rows: proposed.rows };
}
