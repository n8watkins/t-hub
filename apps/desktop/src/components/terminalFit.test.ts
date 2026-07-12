// The resize-path floor: saneFitProposal must NEVER return a sub-floor geometry,
// so a parked/hidden/not-yet-laid-out tile can never push ~2 cols to the PTY and
// wedge the whole tmux window (the "2x24 client" bug). These are pure-function
// tests over duck-typed term/fit doubles — no xterm/jsdom canvas needed.
import { describe, it, expect } from "vitest";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import { saneFitProposal, MIN_SANE_COLS, MIN_SANE_ROWS } from "./terminalFit";

// Minimal doubles: saneFitProposal only touches `term.element.parentElement`'s
// offsetWidth/clientWidth and `fit.proposeDimensions()`.
function makeTerm(parentWidth: number | null): Terminal {
  if (parentWidth === null) {
    return { element: undefined } as unknown as Terminal;
  }
  return {
    element: {
      parentElement: { offsetWidth: parentWidth, clientWidth: parentWidth },
    },
  } as unknown as Terminal;
}

function makeFit(
  proposal: { cols: number; rows: number } | undefined | (() => never),
): FitAddon {
  return {
    proposeDimensions:
      typeof proposal === "function" ? proposal : () => proposal,
  } as unknown as FitAddon;
}

describe("saneFitProposal", () => {
  it("returns the proposal for a laid-out tile at a sane size", () => {
    const got = saneFitProposal(makeTerm(1200), makeFit({ cols: 124, rows: 74 }));
    expect(got).toEqual({ cols: 124, rows: 74 });
  });

  it("returns null when the tile has no element yet", () => {
    expect(saneFitProposal(makeTerm(null), makeFit({ cols: 124, rows: 74 }))).toBeNull();
  });

  it("returns null for a zero-width (parked/hidden/pre-layout) tile", () => {
    // FitAddon on a 0px box would floor at MINIMUM_COLS=2; we must bail first.
    expect(saneFitProposal(makeTerm(0), makeFit({ cols: 2, rows: 24 }))).toBeNull();
  });

  it("returns null for the degenerate 2x24 proposal (the wedge signature)", () => {
    expect(saneFitProposal(makeTerm(1200), makeFit({ cols: 2, rows: 24 }))).toBeNull();
  });

  it("rejects any proposal below the column floor", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: MIN_SANE_COLS - 1, rows: 74 }),
    );
    expect(got).toBeNull();
  });

  it("rejects any proposal below the row floor", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: 124, rows: MIN_SANE_ROWS - 1 }),
    );
    expect(got).toBeNull();
  });

  it("accepts a proposal exactly at the floor", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: MIN_SANE_COLS, rows: MIN_SANE_ROWS }),
    );
    expect(got).toEqual({ cols: MIN_SANE_COLS, rows: MIN_SANE_ROWS });
  });

  it("returns null when FitAddon returns undefined", () => {
    expect(saneFitProposal(makeTerm(1200), makeFit(undefined))).toBeNull();
  });

  it("returns null for non-finite dimensions", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: Number.NaN, rows: 74 }),
    );
    expect(got).toBeNull();
  });

  it("returns null (never throws) when proposeDimensions throws", () => {
    const throwing = makeFit(() => {
      throw new Error("renderer detached");
    });
    expect(saneFitProposal(makeTerm(1200), throwing)).toBeNull();
  });
});
