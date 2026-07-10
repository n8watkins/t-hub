// Terminal output "tail" registry: the mounted <TerminalView>s register their
// xterm instance here, and consumers (the captains-deck orchestrator output
// strip) read the latest non-empty visible line ON DEMAND (polling), so there is
// no per-output-chunk work in the hot render path.
//
// Structural typing only - we never import xterm here, so this stays a tiny,
// dependency-free lib the deck can read without pulling the terminal component.

/** The slice of the xterm API we read: the active buffer's visible rows. */
export interface XtermTailSource {
  rows: number;
  buffer: {
    active: {
      /** Scrollback rows above the viewport (viewport top = baseY). */
      baseY: number;
      getLine(
        y: number,
      ): { translateToString(trimRight?: boolean): string } | undefined;
    };
  };
}

const registry = new Map<string, XtermTailSource>();

/** Register a terminal's xterm instance under its id (called on TerminalView
 *  mount). A later register for the same id replaces the entry (a remount). */
export function registerTerminalTail(id: string, term: XtermTailSource): void {
  registry.set(id, term);
}

/** Drop a terminal's registration (called on unmount). Only deletes when the
 *  current entry is still `term` (guards a remount that registered the new node
 *  before the old one cleaned up). Pass no `term` to force-delete. */
export function unregisterTerminalTail(id: string, term?: XtermTailSource): void {
  if (term === undefined || registry.get(id) === term) registry.delete(id);
}

/**
 * The latest non-empty line currently on the terminal's visible screen: scan the
 * viewport bottom-up and return the first row with text. "" when the id is
 * unknown (not mounted / no terminal) or the screen is blank. Read on demand.
 */
export function readTerminalTailLine(id: string | null | undefined): string {
  if (!id) return "";
  const term = registry.get(id);
  if (!term) return "";
  const active = term.buffer.active;
  const top = active.baseY;
  const bottom = top + term.rows - 1;
  for (let y = bottom; y >= top; y -= 1) {
    const line = active.getLine(y)?.translateToString(true).replace(/\s+$/, "");
    if (line) return line;
  }
  return "";
}

/**
 * The whole VISIBLE screen as one newline-joined string (top row first), so a
 * consumer can pattern-match a MULTI-LINE artifact (e.g. the Claude usage-limit
 * modal, whose banner and numbered options span several rows). Trailing blank
 * rows are dropped; interior blanks are preserved so the text reads as it looks.
 * "" when the id is unknown or the screen is blank. Read on demand (polling) -
 * no per-output-chunk work in the hot render path, same contract as
 * {@link readTerminalTailLine}.
 *
 * Only the viewport is read (not the full scrollback): the dialog we scan for is
 * a live modal occupying the visible screen, and bounding the scan to `rows`
 * keeps each poll O(viewport).
 */
export function readTerminalTailText(id: string | null | undefined): string {
  if (!id) return "";
  const term = registry.get(id);
  if (!term) return "";
  const active = term.buffer.active;
  const top = active.baseY;
  const bottom = top + term.rows - 1;
  const lines: string[] = [];
  for (let y = top; y <= bottom; y += 1) {
    lines.push(
      active.getLine(y)?.translateToString(true).replace(/\s+$/, "") ?? "",
    );
  }
  // Drop trailing blank rows (the cursor usually sits below the content) so the
  // joined text ends at the last real line; keep interior blanks.
  while (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.join("\n");
}
