import { describe, expect, it, vi } from "vitest";

import { TerminalWriteLifecycle } from "./terminalWriteLifecycle";

class FakeTerminal {
  readonly callbacks: Array<() => void> = [];
  readonly write = vi.fn((_data: string | Uint8Array, callback?: () => void) => {
    if (callback) this.callbacks.push(callback);
  });
  readonly dispose = vi.fn();
}

describe("TerminalWriteLifecycle", () => {
  it("defers destructive work until every accepted write is parsed", () => {
    const terminal = new FakeTerminal();
    const lifecycle = new TerminalWriteLifecycle(terminal);
    const clear = vi.fn();

    expect(lifecycle.write("first")).toBe(true);
    expect(lifecycle.write(new Uint8Array([1, 2, 3]))).toBe(true);
    lifecycle.afterWrites(clear);

    expect(clear).not.toHaveBeenCalled();
    terminal.callbacks.shift()?.();
    expect(clear).not.toHaveBeenCalled();
    terminal.callbacks.shift()?.();
    expect(clear).toHaveBeenCalledOnce();
  });

  it("retires only after queued writes finish and refuses later writes", () => {
    const terminal = new FakeTerminal();
    const lifecycle = new TerminalWriteLifecycle(terminal);

    expect(lifecycle.write("in flight")).toBe(true);
    lifecycle.disposeWhenIdle();

    expect(terminal.dispose).not.toHaveBeenCalled();
    expect(lifecycle.write("too late")).toBe(false);
    expect(terminal.write).toHaveBeenCalledTimes(1);

    terminal.callbacks.shift()?.();
    expect(terminal.dispose).toHaveBeenCalledOnce();

    lifecycle.disposeWhenIdle();
    expect(terminal.dispose).toHaveBeenCalledOnce();
  });
});
