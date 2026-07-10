// Auto-continue coverage store — DEFAULT ON, per-tile opt-out.
//
// The store must be watched-by-default (a fresh tile is covered without any
// action) and persist only the opt-OUT set, under the v2 key. It must NOT read
// the old v1 opt-IN map as if it were opt-out.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useAutoContinue } from "./autoContinue";

const V2_KEY = "t-hub.autoContinue.v2";
const V1_KEY = "t-hub.autoContinue.v1";

/** Re-import the store module FRESH so its top-level `load()` runs against
 *  whatever is currently in localStorage — the only way to exercise the real
 *  load() branches (a live store never re-reads storage after import). */
async function freshStore(): Promise<typeof useAutoContinue> {
  vi.resetModules();
  return (await import("./autoContinue")).useAutoContinue;
}

beforeEach(() => {
  localStorage.clear();
  // Reset to a clean, everyone-watched state between cases.
  useAutoContinue.setState({ optedOut: {} });
});

describe("useAutoContinue — default ON, opt-out semantics", () => {
  it("watches every tile by default (no opt-out recorded)", () => {
    expect(useAutoContinue.getState().isWatched("t1")).toBe(true);
    expect(useAutoContinue.getState().optedOut).toEqual({});
  });

  it("toggle opts a tile out, then back in", () => {
    useAutoContinue.getState().toggle("t1");
    expect(useAutoContinue.getState().isWatched("t1")).toBe(false);
    expect(useAutoContinue.getState().optedOut).toEqual({ t1: true });

    useAutoContinue.getState().toggle("t1");
    expect(useAutoContinue.getState().isWatched("t1")).toBe(true);
    expect(useAutoContinue.getState().optedOut).toEqual({});
  });

  it("setWatched(false) opts out; setWatched(true) re-covers", () => {
    useAutoContinue.getState().setWatched("t1", false);
    expect(useAutoContinue.getState().isWatched("t1")).toBe(false);
    useAutoContinue.getState().setWatched("t1", true);
    expect(useAutoContinue.getState().isWatched("t1")).toBe(true);
  });

  it("persists the opt-out set under the v2 key as { optedOut }", () => {
    useAutoContinue.getState().toggle("t1");
    const raw = localStorage.getItem(V2_KEY);
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw as string)).toEqual({ optedOut: { t1: true } });
  });
});

describe("useAutoContinue — persistence load (real load() via fresh import)", () => {
  it("does NOT treat a legacy v1 opt-in map as an opt-out set", async () => {
    // The reviewer's HIGH: prove load() itself ignores v1. Set a v1 opt-in map
    // with t1+t2 enabled and NO v2 key, then re-import so load() actually runs.
    // Under default-ON everyone must be watched — the v1 map is never read as
    // opt-out (t1/t2 are watched, and so is t3 which v1 never mentioned).
    localStorage.setItem(V1_KEY, JSON.stringify({ t1: true, t2: true }));
    expect(localStorage.getItem(V2_KEY)).toBeNull();
    const store = await freshStore();
    expect(store.getState().optedOut).toEqual({}); // v1 not adopted as opt-out
    expect(store.getState().isWatched("t1")).toBe(true);
    expect(store.getState().isWatched("t2")).toBe(true);
    expect(store.getState().isWatched("t3")).toBe(true);
  });

  it("round-trips the v2 { optedOut } shape through load()", async () => {
    localStorage.setItem(V2_KEY, JSON.stringify({ optedOut: { t1: true } }));
    const store = await freshStore();
    expect(store.getState().optedOut).toEqual({ t1: true });
    expect(store.getState().isWatched("t1")).toBe(false); // opted OUT persists
    expect(store.getState().isWatched("t2")).toBe(true); // others still watched
  });

  it("tolerates a bare id→bool map (no { optedOut } wrapper) via load()", async () => {
    localStorage.setItem(V2_KEY, JSON.stringify({ t9: true }));
    const store = await freshStore();
    expect(store.getState().isWatched("t9")).toBe(false);
  });

  it("starts everyone-watched when no v2 key exists via load()", async () => {
    const store = await freshStore();
    expect(store.getState().optedOut).toEqual({});
    expect(store.getState().isWatched("anything")).toBe(true);
  });
});
