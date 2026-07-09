// Usage-limit dialog detection + the SAFE recovery keystrokes.
//
// Two pure, heavily-tested pieces, kept out of the (side-effecting)
// autoContinueMount so they unit-test in isolation:
//
//   1. matchesUsageLimitDialog(text) — does a terminal's visible pane text show
//      the Claude Code "you're out of usage" MODAL (not merely mention "limit")?
//      This drives the pane-text trigger of auto-continue, which runs DEFAULT ON
//      fleet-wide, so a false positive would inject keystrokes into a healthy
//      session mid-work. The match is therefore deliberately SPECIFIC: it demands
//      the co-occurrence of THREE independent anchors — the limit-reached banner,
//      a PAY-to-continue option, and the WAIT-for-reset option — the exact,
//      unmistakable signature of the interactive modal. Ordinary output that just
//      contains the word "limit" (a rate-limit log line, "daily upload limit",
//      etc.) never carries all three, so it never fires.
//
//   2. buildRecoveryInput(text) — the keystrokes to recover a session stuck on
//      that modal: ESC first (dismiss the numbered menu WITHOUT selecting a paid
//      option), then the continue text + Enter. The ESC-first ordering IS the
//      hard guardrail: with the menu dismissed, whatever text follows types at
//      the freed prompt and can never land on "Add funds" / "Switch to Team
//      plan". See buildRecoveryInput for the invariant its tests pin down.

/** ESC — dismisses the modal's numbered selection menu. */
export const ESC = "\x1b";
/** Carriage return — submits the continue text at the prompt. */
export const CR = "\r";

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

// The "you have run out of usage" BANNER lines. Anchored on the modal's specific
// phrasing ("you've hit/reached your … limit", "usage limit reached", a window
// limit that "resets") rather than a bare "limit", so an app's "daily upload
// limit" or a "rate limit exceeded" log line does not qualify on its own.
const LIMIT_BANNERS: RegExp[] = [
  /you'?ve\s+(?:hit|reached)\s+your\s+[^\n]*\blimit\b/i,
  /\busage\s+limit\s+reached\b/i,
  /\b(?:session|weekly|5-?hour|five-?hour)\s+limit\b[^\n]*\breset/i,
];

// The modal's PAY-to-continue options. Their presence is what makes this the
// billing modal specifically (a plain "resets at 3pm" notice has none of these).
const BILLING_OPTIONS: RegExp[] = [
  /\badd\s+funds\b/i,
  /\bbuy\s+more\s+usage\b/i,
  /\bswitch\s+to\s+(?:the\s+|a\s+)?team\s+plan\b/i,
  /\bpurchase\s+(?:more\s+)?(?:usage|credits)\b/i,
];

// The modal's SAFE option — wait for the window to reset. This is the option the
// recovery effectively takes (via ESC, which dismisses to exactly this outcome).
const WAIT_OPTIONS: RegExp[] = [
  /\bstop\s+and\s+wait\b[^\n]*\breset/i,
  /\bwait\s+(?:for\s+)?[^\n]*\blimit\b[^\n]*\breset/i,
  /\bwait\s+until\b[^\n]*\breset/i,
];

/**
 * True when `text` (a terminal's visible pane, e.g. from readTerminalTailText)
 * shows the Claude Code usage-limit MODAL. Requires all THREE anchors together —
 * the limit banner, a billing/pay option, and the wait-for-reset option — so an
 * ordinary line mentioning "limit" (or even a banner alone) does not fire.
 */
export function matchesUsageLimitDialog(text: string | null | undefined): boolean {
  if (!text) return false;
  const hasBanner = LIMIT_BANNERS.some((re) => re.test(text));
  if (!hasBanner) return false;
  const hasBilling = BILLING_OPTIONS.some((re) => re.test(text));
  if (!hasBilling) return false;
  const hasWait = WAIT_OPTIONS.some((re) => re.test(text));
  return hasWait;
}

// ---------------------------------------------------------------------------
// Recovery keystrokes (the hard guardrail)
// ---------------------------------------------------------------------------

/**
 * The keystrokes that recover a session stuck on the usage-limit modal: ESC to
 * DISMISS the numbered menu (never selecting a paid option), then the continue
 * text + Enter so the agent picks the work back up once its window has reset.
 *
 * GUARDRAIL — the recovery must NEVER select a billing/upgrade option. Two
 * invariants, both pinned by tests:
 *   - the sequence ALWAYS begins with ESC, so the menu is gone before any text
 *     is typed: a following character types at the freed prompt and cannot land
 *     on "Add funds" / "Switch to Team plan";
 *   - a blank/whitespace `text` collapses to ESC ALONE (dismiss only) — we never
 *     fall back to injecting a bare digit or a stray Enter that could re-select.
 */
export function buildRecoveryInput(text: string | null | undefined): string {
  const clean = (text ?? "").trim();
  // Dismiss-only when there's nothing meaningful to type: never a bare digit.
  if (!clean) return ESC;
  return ESC + clean + CR;
}
