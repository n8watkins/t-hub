// Stable captain identity precedence (the overlay/deck/sidebar title fix): the
// derivation must be user rename -> cwd basename -> workspace tab name -> short
// id, and NEVER the volatile Claude session title. cwd beats the tab name
// because a tab is a GROUPING - several unrelated captains can share one tab, so
// the tab name is not a per-captain identity. This pins the precedence at the
// pure-function level.
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

  it("prefers the cwd basename OVER the workspace tab name", () => {
    // The bug: a tab is a grouping, not an identity. The cwd distinguishes.
    expect(
      stableCaptainIdentity(undefined, "appturnity", "/home/n/appturnity/monorepo-app", "abcd1234ef"),
    ).toBe("monorepo-app");
  });

  it("gives two captains sharing ONE tab distinct names from their cwds", () => {
    // The reproduced case: three unrelated captains in the "appturnity" tab.
    // Preferring the tab name collapsed them all to "appturnity"; cwd-first
    // keeps them apart.
    const tab = "appturnity";
    const orchestrator = stableCaptainIdentity(undefined, tab, "/home/n/.t-hub/orchestrator", "id1");
    const thub = stableCaptainIdentity(undefined, tab, "/home/n/projects/tools/t-hub/t-hub-app", "id2");
    const app = stableCaptainIdentity(undefined, tab, "/home/n/appturnity/monorepo-app", "id3");
    expect(orchestrator).toBe("orchestrator");
    expect(thub).toBe("t-hub-app");
    expect(app).toBe("monorepo-app");
    // All distinct - no collapse to the shared tab name.
    expect(new Set([orchestrator, thub, app]).size).toBe(3);
  });

  it("falls back to the workspace tab name when there is no rename or cwd", () => {
    expect(
      stableCaptainIdentity(undefined, "Workspace 1", undefined, "abcd1234ef"),
    ).toBe("Workspace 1");
  });

  it("reads the cwd basename tolerant of trailing slash / either separator", () => {
    expect(stableCaptainIdentity(undefined, undefined, "/repo/api", "abcd1234ef")).toBe(
      "api",
    );
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
    // Blank rename + blank tab, only a cwd -> cwd basename.
    expect(stableCaptainIdentity("   ", "  ", "/repo/api", "id")).toBe("api");
    // Blank rename + blank cwd -> the tab name (next in order).
    expect(stableCaptainIdentity("   ", "Workspace 2", "", "id")).toBe("Workspace 2");
  });

  it("has no parameter for the Claude title - it can never leak in", () => {
    // The signature is (userLabel, workspaceName, cwd, terminalId): there is no
    // title arg, so a volatile Claude title (e.g. "task notification") is
    // structurally impossible to surface. A captain with only a cwd shows the
    // cwd basename, never a title.
    expect(stableCaptainIdentity(undefined, undefined, "/home/proj", "id")).toBe("proj");
  });
});
