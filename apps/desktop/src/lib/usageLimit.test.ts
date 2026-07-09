// Usage-limit dialog detection + the SAFE recovery keystrokes.
//
// This feature runs DEFAULT ON fleet-wide, so the two properties that matter are
// (1) the pane-text match is SPECIFIC — the real modal fires, ordinary output
// that merely says "limit" does NOT — and (2) the recovery keystrokes can never
// select a paid billing option. Both are pinned below.
import { describe, it, expect } from "vitest";
import {
  matchesUsageLimitDialog,
  buildRecoveryInput,
  ESC,
  CR,
} from "./usageLimit";

// A faithful capture of the interactive Claude Code usage-limit modal: a banner
// line, then the numbered recovery menu (pay options + the wait option). The
// leading "❯"/box-drawing residue mirrors what the TUI renders into the pane.
const MODAL = [
  "╭──────────────────────────────────────────────╮",
  "│ You've hit your usage limit.                   │",
  "│                                                │",
  "│ ❯ 1. Add funds to continue with usage credits  │",
  "│   2. Switch to Team plan                       │",
  "│   3. Stop and wait for the limit to reset      │",
  "╰──────────────────────────────────────────────╯",
].join("\n");

// A session-limit variant that names the reset time on the banner line.
const MODAL_SESSION = [
  "You've reached your session limit - resets 3:00pm (America/Los_Angeles)",
  "  1. Buy more usage to continue",
  "  2. Stop and wait for the limit to reset",
].join("\n");

describe("matchesUsageLimitDialog — TRUE POSITIVES (the real modal fires)", () => {
  it("matches the full interactive billing modal", () => {
    expect(matchesUsageLimitDialog(MODAL)).toBe(true);
  });

  it("matches the session-limit variant (reset time on the banner)", () => {
    expect(matchesUsageLimitDialog(MODAL_SESSION)).toBe(true);
  });

  it("is case-insensitive and tolerant of extra whitespace", () => {
    const text = [
      "YOU'VE HIT YOUR USAGE LIMIT",
      "1.   ADD   FUNDS   to continue",
      "2.   stop  and  wait  for the limit to reset",
    ].join("\n");
    expect(matchesUsageLimitDialog(text)).toBe(true);
  });
});

describe("matchesUsageLimitDialog — TRUE NEGATIVES (healthy output never fires)", () => {
  it("does not fire on empty / nullish input", () => {
    expect(matchesUsageLimitDialog("")).toBe(false);
    expect(matchesUsageLimitDialog(null)).toBe(false);
    expect(matchesUsageLimitDialog(undefined)).toBe(false);
  });

  it("does not fire on ordinary output that merely mentions a limit", () => {
    expect(
      matchesUsageLimitDialog("Error: API rate limit exceeded (limit: 1000/min)"),
    ).toBe(false);
    expect(
      matchesUsageLimitDialog("git push rejected: pack exceeds size limit, retrying"),
    ).toBe(false);
    expect(
      matchesUsageLimitDialog("You've reached your daily limit of 5 uploads."),
    ).toBe(false);
  });

  it("does not fire on a banner ALONE (no recovery menu present)", () => {
    expect(matchesUsageLimitDialog("You've hit your usage limit.")).toBe(false);
  });

  it("does not fire on a billing phrase ALONE (no limit banner)", () => {
    expect(
      matchesUsageLimitDialog("Run `/upgrade` to add funds to continue any time."),
    ).toBe(false);
  });

  it("does not fire on a banner + billing option but NO wait option", () => {
    // Missing the third anchor (the wait/reset option) — still not the modal.
    const text = ["You've hit your usage limit.", "1. Add funds to continue"].join(
      "\n",
    );
    expect(matchesUsageLimitDialog(text)).toBe(false);
  });

  it("does not fire on a plain 'resets at' notice with no pay option", () => {
    expect(
      matchesUsageLimitDialog(
        "Claude usage limit reached. Your limit will reset at 3pm.",
      ),
    ).toBe(false);
  });
});

describe("buildRecoveryInput — the paid-option guardrail", () => {
  it("dismisses with ESC first, then types the continue text + Enter", () => {
    expect(buildRecoveryInput("continue")).toBe(ESC + "continue" + CR);
  });

  it("ALWAYS begins with ESC, so the numbered menu is gone before any text", () => {
    for (const text of ["continue", "keep going", "1", "2. Switch to Team plan", "  go  "]) {
      expect(buildRecoveryInput(text).startsWith(ESC)).toBe(true);
    }
  });

  it("collapses a blank/whitespace text to ESC ALONE — never a bare digit or stray Enter", () => {
    expect(buildRecoveryInput("")).toBe(ESC);
    expect(buildRecoveryInput("   ")).toBe(ESC);
    expect(buildRecoveryInput(null)).toBe(ESC);
    expect(buildRecoveryInput(undefined)).toBe(ESC);
  });

  it("can NEVER select a paid menu option: no output is a bare digit submission", () => {
    // Even an adversarial digit text is ESC-prefixed: the menu is dismissed
    // first, so the digit types at the freed prompt — it cannot select option 1
    // ("Add funds") or option 2 ("Switch to Team plan").
    for (const digit of ["1", "2", "3", " 1 "]) {
      const out = buildRecoveryInput(digit);
      expect(out.startsWith(ESC)).toBe(true);
      // Not a menu selection: never "<digit>\r" or "<digit>" without the ESC.
      expect(out).not.toBe(digit.trim() + CR);
      expect(out).not.toBe(digit.trim());
    }
  });

  it("trims surrounding whitespace from the continue text", () => {
    expect(buildRecoveryInput("  continue  ")).toBe(ESC + "continue" + CR);
  });
});
