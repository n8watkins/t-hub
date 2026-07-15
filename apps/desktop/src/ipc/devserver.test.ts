import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

async function loadDevServer() {
  return import("./devserver");
}

beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
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

  it("does not probe or rewrite a non-loopback URL", async () => {
    const { reachablePreviewUrl } = await loadDevServer();

    await expect(
      reachablePreviewUrl("https://preview.example.test/app"),
    ).resolves.toBe("https://preview.example.test/app");
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});
