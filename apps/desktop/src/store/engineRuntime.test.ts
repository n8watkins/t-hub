// The managed-engine runtime store: hydrate from the command, and apply pushed
// snapshots (the supervisor's control://event stream). Transient - never
// persisted.
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("../ipc/engine", () => ({
  engineRuntimeStatus: vi.fn(),
}));

import { engineRuntimeStatus, type EngineRuntimeStatus } from "../ipc/engine";
import { useEngineRuntime } from "./engineRuntime";

const MANAGED_GREEN: EngineRuntimeStatus = {
  managed: true,
  selectedEngine: "kokoro",
  activeEngine: "kokoro",
  degraded: false,
  level: "green",
  kokoro: "up",
  piper: "unknown",
};

beforeEach(() => {
  vi.mocked(engineRuntimeStatus).mockReset();
  useEngineRuntime.setState({ status: null });
});

describe("engineRuntime store", () => {
  it("load() hydrates the snapshot from the command", async () => {
    vi.mocked(engineRuntimeStatus).mockResolvedValue(MANAGED_GREEN);
    await useEngineRuntime.getState().load();
    expect(useEngineRuntime.getState().status).toEqual(MANAGED_GREEN);
  });

  it("load() leaves status null when there's no backend (command rejects)", async () => {
    vi.mocked(engineRuntimeStatus).mockRejectedValue(new Error("no backend"));
    await useEngineRuntime.getState().load();
    expect(useEngineRuntime.getState().status).toBeNull();
  });

  it("apply() adopts a pushed snapshot (e.g. a fallback event)", () => {
    const fallen: EngineRuntimeStatus = {
      ...MANAGED_GREEN,
      activeEngine: "piper",
      degraded: true,
      level: "amber",
      kokoro: "down",
      piper: "up",
    };
    useEngineRuntime.getState().apply(fallen);
    expect(useEngineRuntime.getState().status?.level).toBe("amber");
    expect(useEngineRuntime.getState().status?.activeEngine).toBe("piper");
  });
});
