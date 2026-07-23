import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  beginTerminalDetach,
  resetTerminalDetachmentsForTests,
  TerminalLifecycleController,
  TERMINAL_COLD_AFTER_MS,
  waitForTerminalDetach,
} from "./terminalLifecycle";

beforeEach(() => {
  vi.useFakeTimers();
  resetTerminalDetachmentsForTests();
});

afterEach(() => vi.useRealTimers());

describe("TerminalLifecycleController", () => {
  it("moves parked terminals from warm to cold after the grace period", () => {
    const changed = vi.fn();
    const lifecycle = new TerminalLifecycleController(changed, 100);
    lifecycle.reconcile(["active", "parked"], new Set(["active"]));

    expect(lifecycle.temperature("active", true)).toBe("hot");
    expect(lifecycle.temperature("parked", false)).toBe("warm");

    vi.advanceTimersByTime(100);

    expect(lifecycle.temperature("parked", false)).toBe("cold");
    expect(changed).toHaveBeenCalledTimes(1);
  });

  it("cancels cooling when a terminal returns before the deadline", () => {
    const lifecycle = new TerminalLifecycleController(() => {}, 100);
    lifecycle.reconcile(["term"], new Set());
    vi.advanceTimersByTime(99);
    lifecycle.reconcile(["term"], new Set(["term"]));
    vi.advanceTimersByTime(1);

    expect(lifecycle.temperature("term", true)).toBe("hot");
  });

  it("rehydrates a cold terminal immediately when it becomes hot", () => {
    const lifecycle = new TerminalLifecycleController(() => {}, 100);
    lifecycle.reconcile(["term"], new Set());
    vi.advanceTimersByTime(100);

    expect(lifecycle.temperature("term", false)).toBe("cold");
    expect(lifecycle.temperature("term", true)).toBe("hot");

    lifecycle.reconcile(["term"], new Set(["term"]));
    expect(lifecycle.temperature("term", false)).toBe("warm");
  });

  it("uses the generous default grace so routine tab-switching stays warm", () => {
    // The default (no coldAfterMs override, as TerminalPool constructs it) keeps a
    // parked terminal warm well past a typical switch-away-and-back, so revisiting
    // a tab doesn't reload it. Guards against silently shortening the grace back to
    // the old 30s that made terminals feel like they "constantly refreshed".
    expect(TERMINAL_COLD_AFTER_MS).toBeGreaterThanOrEqual(120_000);

    const lifecycle = new TerminalLifecycleController(() => {});
    lifecycle.reconcile(["term"], new Set());
    vi.advanceTimersByTime(60_000);
    expect(lifecycle.temperature("term", false)).toBe("warm");

    vi.advanceTimersByTime(TERMINAL_COLD_AFTER_MS - 60_000);
    expect(lifecycle.temperature("term", false)).toBe("cold");
  });

  it("forgets removed terminals and cancels their timers", () => {
    const changed = vi.fn();
    const lifecycle = new TerminalLifecycleController(changed, 100);
    lifecycle.reconcile(["term"], new Set());
    lifecycle.reconcile([], new Set());
    vi.advanceTimersByTime(100);

    expect(changed).not.toHaveBeenCalled();
    expect(lifecycle.temperature("term", false)).toBe("warm");
  });

  it("caps parked warm terminals while preserving hot headroom", () => {
    const changed = vi.fn();
    const lifecycle = new TerminalLifecycleController(changed, 300_000, 2);
    lifecycle.reconcile(["hot", "a", "b", "c"], new Set(["hot"]));
    expect(lifecycle.temperature("hot", true)).toBe("hot");
    expect(lifecycle.temperature("a", false)).toBe("cold");
    expect(lifecycle.temperature("b", false)).toBe("warm");
    expect(lifecycle.temperature("c", false)).toBe("warm");
    expect(changed).toHaveBeenCalledTimes(1);

    // The cap is stable after React performs the requested follow-up render.
    lifecycle.reconcile(["hot", "a", "b", "c"], new Set(["hot"]));
    expect(changed).toHaveBeenCalledTimes(1);
  });
});

describe("terminal detach barrier", () => {
  it("waits for an in-flight parking detach before reattach proceeds", async () => {
    let release: (() => void) | undefined;
    const detach = beginTerminalDetach(
      "term",
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    let resumed = false;
    const wait = waitForTerminalDetach("term").then(() => {
      resumed = true;
    });

    await vi.advanceTimersByTimeAsync(0);
    expect(resumed).toBe(false);

    release?.();
    await detach;
    await wait;
    expect(resumed).toBe(true);
  });

  it("serializes repeated detach requests for one terminal", async () => {
    const order: string[] = [];
    const first = beginTerminalDetach("term", async () => {
      order.push("first");
    });
    const second = beginTerminalDetach("term", async () => {
      order.push("second");
    });

    await first;
    await second;
    expect(order).toEqual(["first", "second"]);
  });
});
