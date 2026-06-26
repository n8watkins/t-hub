import { describe, it, expect } from "vitest";
import {
  chordFromEvent,
  bareKeyFromEvent,
  normalizeChord,
  formatChord,
} from "./chord";

// The chord functions only read `.key` and the four modifier booleans off a
// KeyboardEvent, so a small struct cast to KeyboardEvent is a faithful synthetic
// event — no jsdom dispatch needed.
type KeyInit = {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
};

function ev(init: KeyInit): KeyboardEvent {
  return {
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    ...init,
  } as KeyboardEvent;
}

describe("chordFromEvent", () => {
  it("returns the bare key when no modifiers are held", () => {
    expect(chordFromEvent(ev({ key: "t" }))).toBe("t");
  });

  it("lowercases letter keys (Shift+T reports key 'T')", () => {
    expect(chordFromEvent(ev({ key: "T", shiftKey: true }))).toBe("shift+t");
  });

  it("prefixes ctrl for a ctrlKey event", () => {
    expect(chordFromEvent(ev({ key: "t", ctrlKey: true }))).toBe("ctrl+t");
  });

  it("folds Cmd/Meta into ctrl", () => {
    expect(chordFromEvent(ev({ key: "t", metaKey: true }))).toBe("ctrl+t");
  });

  it("does not double 'ctrl' when both ctrlKey and metaKey are set", () => {
    expect(chordFromEvent(ev({ key: "t", ctrlKey: true, metaKey: true }))).toBe(
      "ctrl+t",
    );
  });

  it("emits modifiers in canonical order ctrl, shift, alt", () => {
    expect(
      chordFromEvent(
        ev({ key: "tab", ctrlKey: true, shiftKey: true, altKey: true }),
      ),
    ).toBe("ctrl+shift+alt+tab");
  });

  it("lowercases named keys (Tab -> tab, Escape -> escape)", () => {
    expect(chordFromEvent(ev({ key: "Tab" }))).toBe("tab");
    expect(chordFromEvent(ev({ key: "Escape" }))).toBe("escape");
    expect(chordFromEvent(ev({ key: "ArrowUp" }))).toBe("arrowup");
  });

  it("spells a literal '+' key as 'plus' so it stays parseable", () => {
    expect(chordFromEvent(ev({ key: "+", shiftKey: true }))).toBe("shift+plus");
  });

  it("spells a literal space key as 'space'", () => {
    expect(chordFromEvent(ev({ key: " ", ctrlKey: true }))).toBe("ctrl+space");
  });

  it("keeps digits and punctuation literal", () => {
    expect(chordFromEvent(ev({ key: "1", ctrlKey: true }))).toBe("ctrl+1");
    expect(chordFromEvent(ev({ key: "=", ctrlKey: true }))).toBe("ctrl+=");
  });

  it("rejects lone modifier keydowns (Control/Shift/Alt/Meta/OS)", () => {
    for (const key of ["Control", "Shift", "Alt", "Meta", "OS"]) {
      expect(chordFromEvent(ev({ key }))).toBeNull();
    }
  });

  it("rejects an event with an empty key", () => {
    expect(chordFromEvent(ev({ key: "" }))).toBeNull();
  });
});

describe("bareKeyFromEvent", () => {
  it("returns the normalized key ignoring all modifiers", () => {
    expect(
      bareKeyFromEvent(ev({ key: "t", ctrlKey: true, shiftKey: true })),
    ).toBe("t");
  });

  it("lowercases named keys", () => {
    expect(bareKeyFromEvent(ev({ key: "Escape" }))).toBe("escape");
  });

  it("normalizes '+' to 'plus' and ' ' to 'space'", () => {
    expect(bareKeyFromEvent(ev({ key: "+" }))).toBe("plus");
    expect(bareKeyFromEvent(ev({ key: " " }))).toBe("space");
  });

  it("rejects lone modifier presses and empty keys", () => {
    for (const key of ["Control", "Shift", "Alt", "Meta", "OS", ""]) {
      expect(bareKeyFromEvent(ev({ key }))).toBeNull();
    }
  });
});

describe("normalizeChord", () => {
  it("returns '' for empty input", () => {
    expect(normalizeChord("")).toBe("");
  });

  it("lowercases and trims tokens", () => {
    expect(normalizeChord("  CTRL + T  ")).toBe("ctrl+t");
  });

  it("folds cmd/meta/control aliases into ctrl", () => {
    expect(normalizeChord("cmd+t")).toBe("ctrl+t");
    expect(normalizeChord("meta+t")).toBe("ctrl+t");
    expect(normalizeChord("control+t")).toBe("ctrl+t");
  });

  it("folds option into alt", () => {
    expect(normalizeChord("option+t")).toBe("alt+t");
  });

  it("reorders modifiers into canonical ctrl, shift, alt order", () => {
    expect(normalizeChord("alt+shift+ctrl+tab")).toBe("ctrl+shift+alt+tab");
  });

  it("dedupes repeated modifiers (e.g. ctrl+cmd -> ctrl)", () => {
    expect(normalizeChord("ctrl+cmd+t")).toBe("ctrl+t");
  });

  it("returns '' for a modifiers-only chord (no usable key)", () => {
    expect(normalizeChord("ctrl+shift")).toBe("");
    expect(normalizeChord("ctrl")).toBe("");
  });

  it("cannot recover a literal '+' from a string (joiner is filtered away)", () => {
    // Unlike chordFromEvent (which sees key === "+" directly), normalizeChord
    // splits on "+" and drops empty tokens, so "ctrl++" loses the literal "+"
    // and degenerates to a modifiers-only chord -> "". A literal-plus binding
    // must therefore be spelled "ctrl+plus" in string form (not "ctrl++").
    expect(normalizeChord("ctrl++")).toBe("");
    expect(normalizeChord("ctrl+plus")).toBe("ctrl+plus");
  });

  it("is idempotent: normalizing a canonical chord is a no-op", () => {
    for (const c of ["ctrl+t", "ctrl+shift+tab", "ctrl+1", "alt+t"]) {
      expect(normalizeChord(c)).toBe(c);
    }
  });
});

describe("formatChord", () => {
  it("renders an em dash for an empty chord", () => {
    expect(formatChord("")).toBe("—");
  });

  it("renders ctrl as 'Ctrl/Cmd' to reflect cross-platform folding", () => {
    expect(formatChord("ctrl+t")).toBe("Ctrl/Cmd + T");
  });

  it("pretty-prints named tokens", () => {
    expect(formatChord("ctrl+shift+tab")).toBe("Ctrl/Cmd + Shift + Tab");
    expect(formatChord("escape")).toBe("Esc");
    expect(formatChord("alt+space")).toBe("Alt + Space");
    expect(formatChord("shift+plus")).toBe("Shift + +");
  });

  it("uppercases single-char keys and capitalizes multi-char ones", () => {
    expect(formatChord("ctrl+t")).toContain("T");
    expect(formatChord("arrowup")).toBe("Arrowup");
  });
});

describe("round-trips", () => {
  it("chordFromEvent output is already canonical (normalizeChord is a no-op)", () => {
    const cases: KeyboardEvent[] = [
      ev({ key: "t", ctrlKey: true }),
      ev({ key: "Tab", ctrlKey: true, shiftKey: true }),
      ev({ key: "1", metaKey: true }),
      ev({ key: "+", shiftKey: true }),
    ];
    for (const e of cases) {
      const chord = chordFromEvent(e);
      expect(chord).not.toBeNull();
      expect(normalizeChord(chord!)).toBe(chord);
    }
  });
});
