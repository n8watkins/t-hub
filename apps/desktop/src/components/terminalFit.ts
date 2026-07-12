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

// The RATIFIED readable floor for a BACKGROUND / unwatched tile. A tile the user
// is actively looking at is fit to its real box (down to MIN_SANE_COLS — a narrow
// split is legitimate). But a tile that is NOT on the active surface must never be
// shrunk below a readable width: its agent's output (and a captain's capture-pane
// read of it) wraps to garbage otherwise. So for an unwatched tile we raise the
// acceptance floor to 80 — a proposal narrower than this is REFUSED (returns null,
// exactly like the sub-floor guard), so the window keeps its current ≥80 geometry
// (the deterministic 80x24 spawn seed for a never-viewed tile). This is the
// client-tracked half of the retired manual-220x50 doctrine: floor, don't force —
// so it can never resurrect the 2-col wedge (floor is 80) or the 220 overshoot (we
// never GROW past the real box; a watched tile still fits exactly). Respects #65's
// reassert-window-size-latest design: refusing a shrink is the same "leave it be"
// mechanism, and foregrounding the tile re-fits it to the viewer's real box.
export const MIN_READABLE_COLS = 80;

/**
 * The FitAddon's proposed geometry, but ONLY when the tile is genuinely laid out
 * at a readable size. Returns `null` when the tile is unmeasurable (no element
 * yet, zero-width box) or the proposal is degenerate/sub-floor — the caller must
 * then NOT fit or resize, leaving the terminal at its current (sane) geometry
 * until it gets a real box (the ResizeObserver / pool-move re-fits it then). This
 * is the single guard that keeps a background/pre-layout tile from ever reporting
 * ~2 cols to the PTY.
 *
 * `minCols` is the column acceptance floor (default {@link MIN_SANE_COLS}). Pass
 * {@link MIN_READABLE_COLS} for a BACKGROUND / unwatched tile so it is never
 * shrunk below a readable width (see that constant). A watched/foreground tile
 * uses the default so it fits its real box exactly.
 */
export function saneFitProposal(
  term: Terminal,
  fit: FitAddon,
  minCols: number = MIN_SANE_COLS,
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
    proposed.cols < minCols ||
    proposed.rows < MIN_SANE_ROWS
  ) {
    return null;
  }
  return { cols: proposed.cols, rows: proposed.rows };
}
