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
//
// VERIFIED against the real Claude Code modal: the banner is built from
// `"You've hit your ${H}"` with H ∈ {"usage limit", "session limit", "weekly
// limit", "Opus limit", "monthly spend limit", "limit"} (optionally followed by
// " · resets …"), plus the "usage limit reached" error string. Regex #1 covers
// every "You've hit your <window> limit" instance; #2 covers "usage limit
// reached". (See REPORT.md for the read-only capture of these strings.)
const LIMIT_BANNERS: RegExp[] = [
  /you'?ve\s+(?:hit|reached)\s+your\s+[^\n]*\blimit\b/i,
  /\busage\s+limit\s+reached\b/i,
  /\b(?:session|weekly|5-?hour|five-?hour)\s+limit\b[^\n]*\breset/i,
];

// The modal's PAY-to-continue options. Their presence is what makes this the
// billing modal specifically (a plain "resets at 3pm" notice has none of these).
//
// VERIFIED against the real modal's menu labels: "Add funds to continue with
// usage credits", "Switch to Team plan" / "Upgrade to Team plan", "Upgrade your
// plan" / "Upgrade to Max", "Buy more". Any ONE of these (alongside the banner +
// wait anchors) marks the interactive billing modal.
const BILLING_OPTIONS: RegExp[] = [
  /\badd\s+funds\b/i,
  /\bbuy\s+more\b/i,
  /\b(?:switch|upgrade)\s+to\s+(?:the\s+|a\s+)?team\s+plan\b/i,
  /\bupgrade\s+(?:your\s+plan|to\s+(?:the\s+)?max\b)/i,
  /\bpurchase\s+(?:more\s+)?(?:usage|credits)\b/i,
];

// The modal's SAFE option — wait for the window to reset. This is the option the
// recovery effectively takes (via ESC, which dismisses to exactly this outcome).
//
// VERIFIED against the real labels "Stop and wait for limit to reset" and "Wait
// for limit to reset". (When the modal has no reset time this option collapses to
// a bare "Stop"; that hard-cap variant is not our target — waiting can't unblock
// it — and the timer trigger, not the pane trigger, remains its safety net.)
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
 * The continue text, cleaned for safe injection: outer whitespace trimmed AND all
 * interior C0/DEL control chars (`/[\x00-\x1f\x7f]/`) stripped. This is the second
 * half of the guardrail: after the leading ESC dismiss, the ONLY control bytes we
 * ever emit are that ESC and the single trailing CR. A raw setting like "go\rx"
 * would otherwise inject an extra Enter mid-sequence (a stray submission), and an
 * interior ESC could form an arrow/CSI sequence that NAVIGATES the menu onto a
 * paid option — both are removed here at the source.
 */
export function sanitizeContinueText(text: string | null | undefined): string {
  // eslint-disable-next-line no-control-regex -- deliberately stripping control bytes
  return (text ?? "").trim().replace(/[\x00-\x1f\x7f]/g, "");
}

/**
 * The keystrokes that recover a session stuck on the usage-limit modal: ESC to
 * DISMISS the numbered menu (never selecting a paid option), then the continue
 * text + Enter so the agent picks the work back up once its window has reset.
 *
 * GUARDRAIL — the recovery must NEVER select a billing/upgrade option. Three
 * invariants, all pinned by tests:
 *   - the sequence ALWAYS begins with ESC, so the menu is gone before any text
 *     is typed: a following character types at the freed prompt and cannot land
 *     on "Add funds" / "Switch to Team plan";
 *   - a blank/whitespace `text` collapses to ESC ALONE (dismiss only) — we never
 *     fall back to injecting a bare digit or a stray Enter that could re-select;
 *   - interior control chars are STRIPPED (sanitizeContinueText), so the only ESC
 *     is the leading dismiss and the only CR is the final submit.
 *
 * NOTE: the live injector (autoContinueMount::recover) does not send this as ONE
 * write — it sends the ESC as its OWN write, waits for the terminal to settle,
 * then sends `text + CR`, via {@link buildRecoverySteps}. This combined form is
 * retained for the byte-contract tests and as the single source of the sequence's
 * shape; the split write removes any ESC-then-CR atomicity assumption.
 */
export function buildRecoveryInput(text: string | null | undefined): string {
  const clean = sanitizeContinueText(text);
  // Dismiss-only when there's nothing meaningful to type: never a bare digit.
  if (!clean) return ESC;
  return ESC + clean + CR;
}

/**
 * The recovery split into its two SEPARATE PTY writes:
 *   - `dismiss` — ESC ALONE, sent first as its own write so the TUI parses it as a
 *     standalone Escape (a dismiss), never folding it into a Meta/CSI sequence
 *     whose trailing CR could select the default-highlighted PAID option;
 *   - `submit`  — the sanitized continue text + CR, sent AFTER a settle delay, or
 *     the empty string when there is nothing to type (dismiss-only recovery).
 *
 * The two are byte-identical to `buildRecoveryInput` when concatenated, so the
 * guardrail invariants hold; splitting them only removes the atomicity assumption.
 */
export function buildRecoverySteps(text: string | null | undefined): {
  dismiss: string;
  submit: string;
} {
  const clean = sanitizeContinueText(text);
  return { dismiss: ESC, submit: clean ? clean + CR : "" };
}
