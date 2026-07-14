import { beforeEach, describe, expect, it, vi } from "vitest";
import { Events, type ExitEvent, type OutputEvent, type StateEvent } from "./types";

type EventPayload = OutputEvent | StateEvent | ExitEvent;
type Listener = (event: { payload: EventPayload }) => void;

const tauri = vi.hoisted(() => ({
  listeners: new Map<string, Listener>(),
  listen: vi.fn(),
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: tauri.listen,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
}));

async function loadClient() {
  return import("./client");
}

function emit(event: string, payload: EventPayload): void {
  const listener = tauri.listeners.get(event);
  if (!listener) throw new Error(`missing backing listener for ${event}`);
  listener({ payload });
}

beforeEach(() => {
  vi.resetModules();
  tauri.listeners.clear();
  tauri.listen.mockReset();
  tauri.invoke.mockReset();
  tauri.listen.mockImplementation(
    async (event: string, callback: Listener): Promise<() => void> => {
      tauri.listeners.set(event, callback);
      return () => tauri.listeners.delete(event);
    },
  );
});

describe("terminal enumeration", () => {
  it("deduplicates concurrent cold-start requests", async () => {
    let resolve: (value: unknown) => void = () => {};
    tauri.invoke.mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      }),
    );
    const { listTerminals } = await loadClient();

    const first = listTerminals();
    const second = listTerminals();
    resolve([]);

    await expect(first).resolves.toEqual([]);
    await expect(second).resolves.toEqual([]);
    expect(tauri.invoke).toHaveBeenCalledTimes(1);
  });

  it("retries one bounded tmux timeout and then clears the in-flight request", async () => {
    tauri.invoke
      .mockRejectedValueOnce(
        "failed to list tmux sessions: command exceeded 10s timeout",
      )
      .mockResolvedValueOnce([{ id: "A" }])
      .mockResolvedValueOnce([]);
    const { listTerminals } = await loadClient();

    await expect(listTerminals()).resolves.toEqual([{ id: "A" }]);
    await expect(listTerminals()).resolves.toEqual([]);
    expect(tauri.invoke).toHaveBeenCalledTimes(3);
  });

  it("does not retry a non-timeout failure", async () => {
    tauri.invoke.mockRejectedValueOnce("permission denied");
    const { listTerminals } = await loadClient();

    await expect(listTerminals()).rejects.toBe("permission denied");
    expect(tauri.invoke).toHaveBeenCalledTimes(1);
  });
});

describe("terminal event fanout", () => {
  it("dispatches output only to subscribers keyed to that terminal", async () => {
    const { onOutput } = await loadClient();
    const callbacks = Array.from({ length: 26 }, () => vi.fn());
    const ids = Array.from({ length: 26 }, (_, index) =>
      String.fromCharCode("A".charCodeAt(0) + index),
    );

    await Promise.all(ids.map((id, index) => onOutput(id, callbacks[index])));
    emit(Events.output, { id: "A", base64: "QQ==" });

    expect(callbacks[0]).toHaveBeenCalledOnce();
    expect(callbacks.slice(1).every((callback) => callback.mock.calls.length === 0)).toBe(
      true,
    );
    expect(tauri.listen).toHaveBeenCalledTimes(1);
  });

  it("supports multiple subscribers for the same terminal in registration order", async () => {
    const { onState } = await loadClient();
    const calls: string[] = [];
    await onState("A", () => calls.push("first"));
    await onState("A", () => calls.push("second"));

    emit(Events.state, { id: "A", state: "live" });

    expect(calls).toEqual(["first", "second"]);
  });

  it("uses a stable snapshot when a subscriber unsubscribes during dispatch", async () => {
    const { onExit } = await loadClient();
    const calls: string[] = [];
    let unsubscribeSecond = () => {};
    await onExit("A", () => {
      calls.push("first");
      unsubscribeSecond();
    });
    unsubscribeSecond = await onExit("A", () => calls.push("second"));

    emit(Events.exit, { id: "A", code: 0 });
    emit(Events.exit, { id: "A", code: 0 });

    expect(calls).toEqual(["first", "second", "first"]);
  });

  it("deduplicates callbacks without allowing stale cleanup to remove a resubscribe", async () => {
    const { onOutput } = await loadClient();
    const callback = vi.fn();
    const unsubscribeFirst = await onOutput("A", callback);
    const unsubscribeDuplicate = await onOutput("A", callback);

    emit(Events.output, { id: "A", base64: "QQ==" });
    unsubscribeFirst();
    const unsubscribeResubscribe = await onOutput("A", callback);
    unsubscribeDuplicate();
    emit(Events.output, { id: "A", base64: "QQ==" });
    unsubscribeResubscribe();
    emit(Events.output, { id: "A", base64: "QQ==" });

    expect(callback).toHaveBeenCalledTimes(2);
  });

  it("preserves global subscriptions and ordering alongside keyed subscriptions", async () => {
    const { onState } = await loadClient();
    const calls: string[] = [];
    await onState(() => calls.push("global-first"));
    await onState("A", () => calls.push("keyed"));
    await onState(() => calls.push("global-last"));

    emit(Events.state, { id: "A", state: "live" });
    emit(Events.state, { id: "B", state: "detached" });

    expect(calls).toEqual([
      "global-first",
      "keyed",
      "global-last",
      "global-first",
      "global-last",
    ]);
    expect(tauri.listen).toHaveBeenCalledTimes(1);
  });
});
