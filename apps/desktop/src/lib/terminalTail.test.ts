// The terminal-tail registry: registration lifecycle + reading the latest
// visible non-empty line from a registered xterm buffer.
import { describe, it, expect, beforeEach } from "vitest";
import {
  registerTerminalTail,
  unregisterTerminalTail,
  readTerminalTailLine,
  type XtermTailSource,
} from "./terminalTail";

function fakeTerm(lines: string[], baseY = 0): XtermTailSource {
  return {
    rows: lines.length,
    buffer: {
      active: {
        baseY,
        getLine: (y: number) =>
          lines[y - baseY] !== undefined
            ? { translateToString: () => lines[y - baseY] }
            : undefined,
      },
    },
  };
}

beforeEach(() => {
  unregisterTerminalTail("a");
  unregisterTerminalTail("b");
});

describe("readTerminalTailLine", () => {
  it("returns the bottom-most non-empty visible line", () => {
    registerTerminalTail("a", fakeTerm(["first", "middle", "last line "]));
    expect(readTerminalTailLine("a")).toBe("last line");
  });

  it("skips trailing blank rows to find the last real output", () => {
    registerTerminalTail("a", fakeTerm(["output here", "", "   "]));
    expect(readTerminalTailLine("a")).toBe("output here");
  });

  it("honors the viewport offset (baseY)", () => {
    // Viewport starts at baseY=100; rows 100..101 are the visible screen.
    registerTerminalTail("a", fakeTerm(["scrolled top", "bottom now"], 100));
    expect(readTerminalTailLine("a")).toBe("bottom now");
  });

  it("returns '' for an unknown / null id", () => {
    expect(readTerminalTailLine("nope")).toBe("");
    expect(readTerminalTailLine(null)).toBe("");
    expect(readTerminalTailLine(undefined)).toBe("");
  });

  it("returns '' after the terminal is unregistered", () => {
    const term = fakeTerm(["live"]);
    registerTerminalTail("b", term);
    expect(readTerminalTailLine("b")).toBe("live");
    unregisterTerminalTail("b", term);
    expect(readTerminalTailLine("b")).toBe("");
  });

  it("unregister only deletes when the entry still matches (remount guard)", () => {
    const older = fakeTerm(["old"]);
    const newer = fakeTerm(["new"]);
    registerTerminalTail("a", older);
    registerTerminalTail("a", newer); // remount replaced it
    unregisterTerminalTail("a", older); // stale unregister must be a no-op
    expect(readTerminalTailLine("a")).toBe("new");
  });
});
