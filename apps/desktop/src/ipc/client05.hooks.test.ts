import { beforeEach, describe, expect, it, vi } from "vitest";
import { Commands05 } from "./types";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
}));

vi.mock("./controlClient", () => ({
  controlRequest: vi.fn(),
  onControlEvent: vi.fn(),
}));

import {
  codexHooksHealth,
  installCodexHooks,
  repairCodexHooks,
  uninstallCodexHooks,
} from "./client05";

beforeEach(() => {
  tauri.invoke.mockReset().mockResolvedValue({});
});

describe("Codex hook IPC", () => {
  it("keeps install and repair explicitly consent-gated", async () => {
    await installCodexHooks("t-hub-agent", true);
    await repairCodexHooks("t-hub-agent", false);

    expect(tauri.invoke).toHaveBeenNthCalledWith(1, Commands05.installCodexHooks, {
      agentBin: "t-hub-agent",
      consent: true,
    });
    expect(tauri.invoke).toHaveBeenNthCalledWith(2, Commands05.repairCodexHooks, {
      agentBin: "t-hub-agent",
      consent: false,
    });
  });

  it("passes the executable to uninstall without fabricating consent", async () => {
    await uninstallCodexHooks("/usr/local/bin/t-hub-agent");

    expect(tauri.invoke).toHaveBeenCalledWith(Commands05.uninstallCodexHooks, {
      agentBin: "/usr/local/bin/t-hub-agent",
    });
  });

  it("uses null when no focused project is available to the health probe", async () => {
    await codexHooksHealth("t-hub-agent");
    await codexHooksHealth("t-hub-agent", "/workspace/project");

    expect(tauri.invoke).toHaveBeenNthCalledWith(1, Commands05.codexHooksHealth, {
      agentBin: "t-hub-agent",
      projectRoot: null,
    });
    expect(tauri.invoke).toHaveBeenNthCalledWith(2, Commands05.codexHooksHealth, {
      agentBin: "t-hub-agent",
      projectRoot: "/workspace/project",
    });
  });
});
