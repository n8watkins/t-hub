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
  buildRecoverySteps,
  sanitizeContinueText,
  ESC,
  CR,
} from "./usageLimit";

// The interactive Claude Code usage-limit modal, using the REAL strings verified
// read-only from the installed Claude Code binary (see REPORT.md): the banner is
// `"You've hit your ${window} limit"` (+ " · resets …"), and the menu labels are
// "Add funds to continue with usage credits", "Switch to Team plan", and "Stop and
// wait for limit to reset" (note: "for limit", not "for the limit"). The leading
// "❯"/box-drawing residue mirrors what the TUI renders into the pane.
const MODAL = [
  "╭──────────────────────────────────────────────╮",
  "│ You've hit your usage limit · resets 3:00pm    │",
  "│                                                │",
  "│ ❯ Add funds to continue with usage credits     │",
  "│   Switch to Team plan                          │",
  "│   Stop and wait for limit to reset             │",
  "╰──────────────────────────────────────────────╯",
].join("\n");

// A weekly-limit variant with the UPGRADE / Buy more / Wait labels (all real).
const MODAL_SESSION = [
  "You've hit your weekly limit · resets Jul 10, 9:00am (America/Los_Angeles)",
  "❯ Upgrade your plan",
  "  Buy more",
  "  Wait for limit to reset",
].join("\n");

describe("matchesUsageLimitDialog — TRUE POSITIVES (the real modal fires)", () => {
  it("matches the full interactive billing modal", () => {
    expect(matchesUsageLimitDialog(MODAL)).toBe(true);
  });

  it("matches the weekly/upgrade variant (Upgrade your plan + Wait to reset)", () => {
    expect(matchesUsageLimitDialog(MODAL_SESSION)).toBe(true);
  });

  it("matches every real banner window type", () => {
    // The real banner is `You've hit your ${H} limit`; H is one of these windows.
    for (const window of [
      "usage",
      "session",
      "weekly",
      "Opus",
      "5-hour",
    ]) {
      const text = [
        `You've hit your ${window} limit · resets soon`,
        "Add funds to continue with usage credits",
        "Stop and wait for limit to reset",
      ].join("\n");
      expect(matchesUsageLimitDialog(text)).toBe(true);
    }
  });

  it("is case-insensitive and tolerant of extra whitespace", () => {
    const text = [
      "YOU'VE HIT YOUR USAGE LIMIT",
      "ADD   FUNDS   to continue",
      "stop  and  wait  for limit to reset",
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

  it("STRIPS interior control chars so no stray ESC/CR survives", () => {
    // An interior CR would inject an extra Enter mid-sequence (a stray, unintended
    // submission); an interior ESC could form an arrow/CSI sequence that navigates
    // the menu onto a paid option. Both are removed at the source.
    expect(sanitizeContinueText("go\rx")).toBe("gox");
    expect(sanitizeContinueText("a\x1bb")).toBe("ab");
    expect(sanitizeContinueText("keep\ngoing")).toBe("keepgoing");
    expect(sanitizeContinueText("tab\tsep")).toBe("tabsep");
    expect(sanitizeContinueText("del\x7fete")).toBe("delete");
    // The built sequence therefore has EXACTLY one CR (the trailing submit) and its
    // only ESC is the leading dismiss — never an interior one.
    const out = buildRecoveryInput("go\rx\r\n");
    expect(out).toBe(ESC + "gox" + CR);
    expect(out.split(CR)).toHaveLength(2); // one CR: the final submit only
    expect(out.indexOf(ESC)).toBe(0); // ESC only at the front
    expect(out.slice(1).includes(ESC)).toBe(false); // no interior ESC
  });
});

describe("buildRecoverySteps — the SPLIT write (ESC standalone, then text)", () => {
  it("emits ESC as its OWN step and the sanitized text + CR as the second", () => {
    expect(buildRecoverySteps("continue")).toEqual({
      dismiss: ESC,
      submit: "continue" + CR,
    });
  });

  it("collapses a blank/whitespace text to a dismiss-only step (no submit)", () => {
    for (const blank of ["", "   ", "\n", "\t", null, undefined]) {
      expect(buildRecoverySteps(blank)).toEqual({ dismiss: ESC, submit: "" });
    }
  });

  it("dismiss is ALWAYS a lone ESC — never ESC+bytes, so it can't fold into CSI", () => {
    for (const text of ["continue", "1", "2. Switch to Team plan", "go\rx"]) {
      const { dismiss, submit } = buildRecoverySteps(text);
      expect(dismiss).toBe(ESC); // the standalone dismiss, on its own write
      expect(submit.includes(ESC)).toBe(false); // the ESC never rides with the text
    }
  });

  it("the submit step carries at most ONE CR (the final Enter), never interior", () => {
    const { submit } = buildRecoverySteps("go\rx\r\n");
    expect(submit).toBe("gox" + CR);
    expect(submit.split(CR)).toHaveLength(2); // trailing CR only
  });

  it("concatenated, the two steps are byte-identical to buildRecoveryInput", () => {
    for (const text of ["continue", "", "  keep going  ", "go\rx", "1"]) {
      const { dismiss, submit } = buildRecoverySteps(text);
      expect(dismiss + submit).toBe(buildRecoveryInput(text));
    }
  });
});
