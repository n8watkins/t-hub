// The resize-path floor: saneFitProposal must NEVER return a sub-floor geometry,
// so a parked/hidden/not-yet-laid-out tile can never push ~2 cols to the PTY and
// wedge the whole tmux window (the "2x24 client" bug). These are pure-function
// tests over duck-typed term/fit doubles — no xterm/jsdom canvas needed.
import { describe, it, expect } from "vitest";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import {
  saneFitProposal,
  MIN_SANE_COLS,
  MIN_SANE_ROWS,
  MIN_READABLE_COLS,
} from "./terminalFit";

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

// The client-tracked readable floor: a BACKGROUND / unwatched tile passes
// MIN_READABLE_COLS as the column floor so it is never shrunk below a readable
// width (its output would wrap to garbage before anyone views it). A WATCHED tile
// keeps the default MIN_SANE_COLS floor and fits its real (possibly narrow) box.
describe("saneFitProposal readable floor (background tiles)", () => {
  it("is 80 — the ratified readable floor", () => {
    expect(MIN_READABLE_COLS).toBe(80);
  });

  it("background floor REFUSES a narrow proposal a foreground tile would accept", () => {
    // A 40-col box: legitimate for a watched narrow split, but a background tile
    // must not shrink to it (returns null -> the caller leaves the ≥80 geometry).
    const term = makeTerm(1200);
    const fit = makeFit({ cols: 40, rows: 40 });
    // Foreground (default MIN_SANE_COLS floor) accepts it.
    expect(saneFitProposal(term, fit)).toEqual({ cols: 40, rows: 40 });
    // Background (readable floor) refuses it.
    expect(saneFitProposal(term, fit, MIN_READABLE_COLS)).toBeNull();
  });

  it("background floor still ACCEPTS a readable (>=80) proposal", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: 120, rows: 40 }),
      MIN_READABLE_COLS,
    );
    // A wide background tile keeps its real width (we floor, never force-shrink).
    expect(got).toEqual({ cols: 120, rows: 40 });
  });

  it("background floor accepts exactly at the readable floor (80)", () => {
    const got = saneFitProposal(
      makeTerm(1200),
      makeFit({ cols: MIN_READABLE_COLS, rows: 24 }),
      MIN_READABLE_COLS,
    );
    expect(got).toEqual({ cols: MIN_READABLE_COLS, rows: 24 });
  });

  it("the readable floor never resurrects the 2-col wedge (parked tile still null)", () => {
    // A 0px background tile bails on the unmeasurable-box guard, same as before —
    // the readable floor only RAISES the acceptance bar, never lowers it.
    expect(
      saneFitProposal(makeTerm(0), makeFit({ cols: 2, rows: 24 }), MIN_READABLE_COLS),
    ).toBeNull();
  });
});
