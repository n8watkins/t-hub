import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  beginTerminalDetach,
  resetTerminalDetachmentsForTests,
  TerminalLifecycleController,
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

  it("forgets removed terminals and cancels their timers", () => {
    const changed = vi.fn();
    const lifecycle = new TerminalLifecycleController(changed, 100);
    lifecycle.reconcile(["term"], new Set());
    lifecycle.reconcile([], new Set());
    vi.advanceTimersByTime(100);

    expect(changed).not.toHaveBeenCalled();
    expect(lifecycle.temperature("term", false)).toBe("warm");
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
