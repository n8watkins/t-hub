import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  getTerminalResourceSnapshot,
  removeTerminalResources,
  resetTerminalResourcesForTests,
  subscribeTerminalResources,
  updateTerminalResources,
} from "./terminalResources";

beforeEach(() => resetTerminalResourcesForTests());

describe("terminal resource counters", () => {
  it("counts lifecycle temperatures and live resources independently", () => {
    updateTerminalResources("hot", {
      temperature: "hot",
      xterm: true,
      canvas: true,
      pty: true,
    });
    updateTerminalResources("warm", {
      temperature: "warm",
      xterm: true,
      canvas: true,
      pty: true,
    });
    updateTerminalResources("cold", { temperature: "cold" });

    expect(getTerminalResourceSnapshot()).toEqual({
      total: 3,
      hot: 1,
      warm: 1,
      cold: 1,
      xterms: 2,
      canvases: 2,
      ptys: 2,
    });
  });

  it("updates one terminal without double-counting and removes it cleanly", () => {
    updateTerminalResources("term", { temperature: "warm", xterm: true });
    updateTerminalResources("term", { temperature: "hot", pty: true });
    removeTerminalResources("term");

    expect(getTerminalResourceSnapshot()).toEqual({
      total: 0,
      hot: 0,
      warm: 0,
      cold: 0,
      xterms: 0,
      canvases: 0,
      ptys: 0,
    });
  });

  it("notifies subscribers only when the observable state changes", () => {
    const listener = vi.fn();
    const unsubscribe = subscribeTerminalResources(listener);

    updateTerminalResources("term", { temperature: "warm" });
    updateTerminalResources("term", { temperature: "warm" });
    updateTerminalResources("term", { pty: true });
    unsubscribe();
    removeTerminalResources("term");

    expect(listener).toHaveBeenCalledTimes(2);
  });
});
