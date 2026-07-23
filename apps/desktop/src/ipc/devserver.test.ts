import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
}));
const control = vi.hoisted(() => ({
  request: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));
vi.mock("./controlClient", () => ({
  controlRequest: control.request,
}));

async function loadDevServer() {
  return import("./devserver");
}

beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
  control.request.mockReset();
});

describe("shared Preview lifecycle", () => {
  const registry = {
    projects: [
      {
        projectId: "project-1",
        rootPath: "/repo",
        repoRoot: "/repo",
      },
    ],
    workspaces: [
      {
        id: "workspace-1",
        kind: "work",
        owner: { projectId: "project-1" },
        tileIds: ["terminal-1"],
      },
    ],
    captains: [],
  };

  it("derives exact durable authority and forwards discovery and start", async () => {
    control.request.mockImplementation(async (command: string) => {
      if (command === "list_captains") return registry;
      if (command === "preview_discover") {
        return {
          discoveryFingerprint: "sha256:discovery",
          targets: [
            {
              id: "root:dev",
              label: "Dev",
              kind: {
                type: "packageScript",
                packageManager: "npm",
                script: "dev",
              },
              relativeRoot: "",
              recommended: true,
            },
          ],
        };
      }
      if (command === "preview_start") {
        return {
          status: {
            state: "running",
            targetId: "root:dev",
            runId: "run-1",
            previewUrl: "http://127.0.0.1:43191/",
            observedAtMs: 42,
          },
        };
      }
      throw new Error(`unexpected command ${command}`);
    });
    const { discoverRunTargets, startDevServer } = await loadDevServer();
    const discovery = await discoverRunTargets("terminal-1", "/repo/apps/web");
    expect(discovery.targets[0]?.id).toBe("root:dev");

    const snapshot = await startDevServer("terminal-1", "/repo/apps/web", {
      kind: "packageScript",
      id: "root:dev",
      script: "dev",
    });
    expect(snapshot.state).toBe("running");
    expect(snapshot.targetId).toBe("root:dev");
    expect(snapshot.previewUrl).toBe("http://127.0.0.1:43191/");
    expect(control.request).toHaveBeenNthCalledWith(2, "preview_discover", {
      rootPath: "/repo",
    });
    expect(control.request).toHaveBeenNthCalledWith(
      4,
      "preview_start",
      expect.objectContaining({
        rootPath: "/repo",
        scope: {
          projectId: "project-1",
          workspaceId: "workspace-1",
        },
        target: {
          scope: {
            projectId: "project-1",
            workspaceId: "workspace-1",
          },
          targetId: "root:dev",
          discoveryFingerprint: "sha256:discovery",
        },
        requestId: expect.any(String),
      }),
    );
  });

  it("refuses an unowned terminal before any Preview operation", async () => {
    control.request.mockResolvedValue({
      projects: registry.projects,
      workspaces: [],
      captains: [],
    });
    const { discoverRunTargets } = await loadDevServer();
    await expect(discoverRunTargets("foreign", "/repo")).rejects.toThrow(
      "owned by a registered Project",
    );
    expect(control.request).toHaveBeenCalledTimes(1);
    expect(control.request).toHaveBeenCalledWith("list_captains");
  });

  it("exposes select, status, refresh, open, restart, and stop parity", async () => {
    const status = {
      state: "stopped",
      observedAtMs: 7,
    };
    control.request.mockImplementation(async (command: string) => {
      if (command === "list_captains") return registry;
      if (command === "preview_discover") {
        return {
          discoveryFingerprint: "sha256:parity",
          targets: [
            {
              id: "static:root",
              label: "Static",
              kind: { type: "staticSite", entrypoint: "index.html" },
              relativeRoot: "",
              recommended: true,
            },
          ],
        };
      }
      if (command === "preview_status") return status;
      return { status };
    });
    const preview = await loadDevServer();
    await preview.discoverRunTargets("terminal-1", "/repo");
    await preview.selectPreviewTarget("terminal-1", "static:root");
    await preview.devServerSnapshot("terminal-1");
    await preview.refreshPreview("terminal-1");
    await preview.openPreview("terminal-1");
    await preview.restartPreview("terminal-1");
    await preview.stopDevServer("terminal-1");
    expect(
      control.request.mock.calls
        .map(([command]) => command)
        .filter((command) => String(command).startsWith("preview_")),
    ).toEqual([
      "preview_discover",
      "preview_select",
      "preview_status",
      "preview_refresh",
      "preview_open",
      "preview_restart",
      "preview_stop",
    ]);
  });
});

describe("reachablePreviewUrl", () => {
  it("keeps localhost when Windows can already reach it", async () => {
    tauri.invoke.mockResolvedValueOnce(true);
    const { reachablePreviewUrl } = await loadDevServer();

    await expect(
      reachablePreviewUrl("http://localhost:1420/path?ready=1"),
    ).resolves.toBe("http://localhost:1420/path?ready=1");
    expect(tauri.invoke).toHaveBeenCalledTimes(1);
    expect(tauri.invoke).toHaveBeenCalledWith("probe_tcp", {
      host: "localhost",
      port: 1420,
      timeoutMs: 500,
    });
  });

  it("rewrites localhost only after the direct route is unreachable", async () => {
    tauri.invoke
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce("172.24.16.1");
    const { reachablePreviewUrl } = await loadDevServer();

    await expect(
      reachablePreviewUrl("http://127.0.0.1:5173/app?q=1"),
    ).resolves.toBe("http://172.24.16.1:5173/app?q=1");
    expect(tauri.invoke).toHaveBeenNthCalledWith(1, "probe_tcp", {
      host: "127.0.0.1",
      port: 5173,
      timeoutMs: 500,
    });
    expect(tauri.invoke).toHaveBeenNthCalledWith(2, "preview_host");
  });

  it("refreshes the backend-derived host after a WSL address change", async () => {
    tauri.invoke
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce("172.24.16.1")
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce("172.24.32.1");
    const { reachablePreviewUrl } = await loadDevServer();

    await expect(
      reachablePreviewUrl("http://127.0.0.1:5173/first"),
    ).resolves.toBe("http://172.24.16.1:5173/first");
    await expect(
      reachablePreviewUrl("http://127.0.0.1:5173/second"),
    ).resolves.toBe("http://172.24.32.1:5173/second");
    expect(tauri.invoke).toHaveBeenNthCalledWith(4, "preview_host");
  });

  it("does not probe or rewrite a non-loopback URL", async () => {
    const { reachablePreviewUrl } = await loadDevServer();

    await expect(
      reachablePreviewUrl("https://preview.example.test/app"),
    ).resolves.toBe("https://preview.example.test/app");
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});
