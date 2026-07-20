import { describe, it, expect, vi } from "vitest";

vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { stableCaptainIdentity } from "./CaptainOverlay";

describe("stableCaptainIdentity durable precedence", () => {
  it("uses the trimmed durable display name", () => {
    expect(stableCaptainIdentity("  Flagship  ", "alpha", "abcd1234ef")).toBe(
      "Flagship",
    );
  });

  it("falls back to the durable ship slug", () => {
    expect(stableCaptainIdentity(undefined, "alpha", "abcd1234ef")).toBe("alpha");
  });

  it("falls back to the short terminal pointer only when durable identity is absent", () => {
    expect(stableCaptainIdentity(undefined, undefined, "abcd1234ef")).toBe(
      "abcd1234",
    );
  });

  it("cannot accept cwd, Workspace, provider title, or focus state", () => {
    expect(stableCaptainIdentity("Alpha Lead", "alpha", "cap00001")).toBe(
      "Alpha Lead",
    );
  });
});
