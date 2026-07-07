// Stable captain identity precedence (the overlay/deck/sidebar title fix): the
// derivation must be user rename -> workspace tab name -> cwd basename -> short
// id, and NEVER the volatile Claude session title. This pins the precedence at
// the pure-function level.
import { describe, it, expect, vi } from "vitest";

// CaptainOverlay imports the terminal pool (xterm color math needs a canvas);
// stub it so importing the pure identity helper stays headless.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { stableCaptainIdentity } from "./CaptainOverlay";

describe("stableCaptainIdentity precedence", () => {
  it("the user rename wins over everything", () => {
    expect(
      stableCaptainIdentity("Flagship", "Workspace 1", "/repo/api", "abcd1234ef"),
    ).toBe("Flagship");
  });

  it("falls back to the workspace tab name before the cwd", () => {
    expect(
      stableCaptainIdentity(undefined, "Workspace 1", "/repo/api", "abcd1234ef"),
    ).toBe("Workspace 1");
  });

  it("falls back to the cwd basename when there is no rename or tab name", () => {
    expect(stableCaptainIdentity(undefined, undefined, "/repo/api", "abcd1234ef")).toBe(
      "api",
    );
    // Tolerant of a trailing slash / either separator.
    expect(stableCaptainIdentity(undefined, undefined, "C:\\work\\proj\\", "id")).toBe(
      "proj",
    );
  });

  it("falls back to the short id when nothing else is available", () => {
    expect(stableCaptainIdentity(undefined, undefined, undefined, "abcd1234ef")).toBe(
      "abcd1234",
    );
  });

  it("treats blank rename / tab name as absent (trimmed)", () => {
    expect(stableCaptainIdentity("   ", "  ", "/repo/api", "id")).toBe("api");
  });

  it("has no parameter for the Claude title - it can never leak in", () => {
    // The signature is (userLabel, workspaceName, cwd, terminalId): there is no
    // title arg, so a volatile Claude title (e.g. "task notification") is
    // structurally impossible to surface. A captain with only a cwd shows the
    // cwd basename, never a title.
    expect(stableCaptainIdentity(undefined, undefined, "/home/proj", "id")).toBe("proj");
  });
});
