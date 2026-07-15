import { describe, expect, it } from "vitest";
import type { CodexUsage } from "../ipc/codex";
import {
  advanceCodexUsage,
  isUsableCodexUsage,
  loadCachedCodexUsage,
  mergeCodexUsage,
} from "./UsageStrip";

const complete: CodexUsage = {
  primary: { usedPercent: 50, windowMinutes: 300, resetsAt: 2_000 },
  secondary: { usedPercent: 19, windowMinutes: 10_080, resetsAt: 9_000 },
  planType: "plus",
  contextTokens: 12_000,
  contextWindow: 258_000,
  ok: true,
};

describe("mergeCodexUsage", () => {
  it("classifies the current primary-only weekly payload without losing session usage", () => {
    const partial: CodexUsage = {
      primary: { usedPercent: 20, windowMinutes: 10_080, resetsAt: 9_100 },
      secondary: null,
      planType: null,
      contextTokens: null,
      contextWindow: null,
      ok: true,
    };

    expect(mergeCodexUsage(complete, partial)).toEqual({
      ...complete,
      secondary: partial.primary,
    });
  });

  it("retains known fields inside a partially populated window", () => {
    const partial: CodexUsage = {
      ...complete,
      primary: { usedPercent: 52, windowMinutes: null, resetsAt: null },
      secondary: null,
    };

    expect(mergeCodexUsage(complete, partial)?.primary).toEqual({
      usedPercent: 52,
      windowMinutes: 300,
      resetsAt: 2_000,
    });
  });

  it("retains a partial primary window beside a recognized weekly window", () => {
    const partial: CodexUsage = {
      ...complete,
      primary: { usedPercent: 52, windowMinutes: null, resetsAt: null },
      secondary: { usedPercent: 20, windowMinutes: 10_080, resetsAt: 9_100 },
    };

    expect(mergeCodexUsage(complete, partial)).toEqual({
      ...complete,
      primary: {
        usedPercent: 52,
        windowMinutes: 300,
        resetsAt: 2_000,
      },
      secondary: partial.secondary,
    });
  });

  it("keeps the previous reading when the provider snapshot is not usable", () => {
    const empty = { ...complete, primary: null, secondary: null };
    expect(isUsableCodexUsage({ ...complete, ok: false })).toBe(false);
    expect(isUsableCodexUsage(empty)).toBe(false);
    expect(mergeCodexUsage(complete, empty)).toBe(complete);
  });

  it("does not resurrect an expired session window after a weekly-only poll", () => {
    const advanced = advanceCodexUsage(complete, 2_001_000);
    const weeklyOnly: CodexUsage = {
      ...complete,
      primary: { usedPercent: 20, windowMinutes: 10_080, resetsAt: 9_100 },
      secondary: null,
    };

    expect(mergeCodexUsage(advanced, weeklyOnly)?.primary?.usedPercent).toBe(0);
  });
});

describe("loadCachedCodexUsage", () => {
  it("migrates the current primary-only weekly cache into the semantic weekly slot", () => {
    const current: CodexUsage = {
      primary: { usedPercent: 50, windowMinutes: 10_080, resetsAt: 9_100 },
      secondary: null,
      planType: "pro",
      contextTokens: null,
      contextWindow: null,
      ok: true,
    };
    localStorage.setItem("t-hub.codexUsage.v1", JSON.stringify(current));

    expect(loadCachedCodexUsage()).toEqual({
      ...current,
      primary: null,
      secondary: current.primary,
    });
  });

  it("keeps a legacy dual-window cache in its semantic slots", () => {
    localStorage.setItem("t-hub.codexUsage.v1", JSON.stringify(complete));
    expect(loadCachedCodexUsage()).toEqual(complete);
  });
});
