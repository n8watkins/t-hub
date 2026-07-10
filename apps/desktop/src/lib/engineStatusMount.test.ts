// The engine-status mount wires the supervisor's control://event pushes to the
// store (degraded state) and to notify() (fallback/recovery toast). The toast
// leg is the "never silent" guarantee for the auto-fallback event.
import { describe, it, expect, vi } from "vitest";

// vi.hoisted so these exist when the (hoisted) vi.mock factory captures them.
const { toastCbs, statusCbs } = vi.hoisted(() => ({
  toastCbs: [] as Array<(t: { kind: "error" | "done"; title: string; body: string }) => void>,
  statusCbs: [] as Array<(s: unknown) => void>,
}));

vi.mock("../ipc/engine", () => ({
  engineRuntimeStatus: vi.fn(() =>
    Promise.resolve({
      managed: false,
      selectedEngine: "piper",
      activeEngine: "piper",
      degraded: false,
      level: "unknown",
      kokoro: "unknown",
      piper: "unknown",
    }),
  ),
  onEngineRuntimeStatus: vi.fn((cb) => {
    statusCbs.push(cb);
    return () => {};
  }),
  onEngineToast: vi.fn((cb) => {
    toastCbs.push(cb);
    return () => {};
  }),
}));
vi.mock("./notify", () => ({ notify: vi.fn() }));

import { notify } from "./notify";
import { useEngineRuntime } from "../store/engineRuntime";
// Importing the module runs its side effect: mountEngineStatus() subscribes.
import "./engineStatusMount";

describe("engineStatusMount", () => {
  it("subscribed to both the status and toast channels", () => {
    expect(statusCbs.length).toBeGreaterThan(0);
    expect(toastCbs.length).toBeGreaterThan(0);
  });

  it("fires notify() with the supervisor's toast (fallback is never silent)", () => {
    toastCbs[0]({ kind: "error", title: "Voice fell back", body: "on Piper" });
    expect(notify).toHaveBeenCalledWith("error", "Voice fell back", "on Piper");
  });

  it("applies a pushed runtime snapshot into the store", () => {
    statusCbs[0]({
      managed: true,
      selectedEngine: "kokoro",
      activeEngine: "piper",
      degraded: true,
      level: "amber",
      kokoro: "down",
      piper: "up",
    });
    expect(useEngineRuntime.getState().status?.level).toBe("amber");
    expect(useEngineRuntime.getState().status?.activeEngine).toBe("piper");
  });
});
