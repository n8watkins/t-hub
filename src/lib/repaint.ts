// Force-repaint-every-terminal broadcast.
//
// WebView2 quirk (the "+ button blanks all terminals" bug): when a new
// full-screen `fixed` overlay layer appears OVER the DOM-rendered xterm grid —
// the spawn-preset menu (Canvas), the file/web preview modal (PreviewOverlay),
// Settings — or is removed again, the terminals underneath can show a
// stale/blank ("muted") frame until something dirties them. They do NOT
// self-heal; previously only a tab switch brought them back. Nothing moved or
// changed size, so neither the pool's IntersectionObserver nor its
// `th-pool-moved` repaint fires.
//
// Fix: any code that toggles such an overlay calls `repaintAllTerminals()`,
// which broadcasts one window event. Every mounted TerminalView listens for it
// and forces an xterm `refresh()` on the next frame (after the overlay's DOM
// change has painted), so toggling any overlay never leaves the grid muted.
import { tlog } from "./diag";

/** The window event TerminalView listens for to force a full repaint. */
export const REPAINT_ALL_EVENT = "th-repaint-all";

/** Ask every mounted terminal to repaint (after the current overlay change
 *  paints). Safe to call from anywhere; a missing `window` (test/SSR) is a
 *  no-op. Logged once per call (not once per terminal) for the diag trail. */
export function repaintAllTerminals(): void {
  if (typeof window === "undefined") return;
  try {
    tlog("repaint", "broadcast th-repaint-all (overlay toggle)");
    window.dispatchEvent(new CustomEvent(REPAINT_ALL_EVENT));
  } catch {
    /* no window / event constructor — nothing to do */
  }
}
