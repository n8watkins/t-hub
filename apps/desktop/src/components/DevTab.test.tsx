import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { usePanels } from "../store/panels";
import type {
  DevServerEvent,
  DevServerSnapshot,
  RunTargetDiscovery,
} from "../ipc/devserver";
import { DevTab, forgetDevState } from "./DevTab";

const mocks = vi.hoisted(() => ({
  discoverRunTargets: vi.fn(),
  devServerSnapshot: vi.fn(),
  startDevServer: vi.fn(),
  stopDevServer: vi.fn(),
  onDevServerEvent: vi.fn(),
  eventHandlers: new Map<string, (event: DevServerEvent) => void>(),
}));

vi.mock("../ipc/devserver", () => ({
  discoverRunTargets: mocks.discoverRunTargets,
  devServerSnapshot: mocks.devServerSnapshot,
  startDevServer: mocks.startDevServer,
  stopDevServer: mocks.stopDevServer,
  onDevServerEvent: mocks.onDevServerEvent,
}));

const discovery: RunTargetDiscovery = {
  state: "ready",
  message: null,
  targets: [
    {
      kind: "packageScript",
      id: "package-script:dev",
      script: "dev",
      label: "dev",
      packageManager: "pnpm",
      commandDisplay: "pnpm run dev",
      recommended: true,
    },
    {
      kind: "packageScript",
      id: "package-script:preview",
      script: "preview",
      label: "preview",
      packageManager: "pnpm",
      commandDisplay: "pnpm run preview",
      recommended: false,
    },
  ],
};

function snapshot(
  terminalId: string,
  state: DevServerSnapshot["state"] = "idle",
  runId: string | null = null,
  revision = 1,
): DevServerSnapshot {
  return {
    terminalId,
    runId,
    revision,
    state,
    target: runId ? discovery.targets[0] : null,
    exitCode: null,
    reason: null,
    previewUrl: null,
    observedAt: 1,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.eventHandlers.clear();
  mocks.discoverRunTargets.mockResolvedValue(discovery);
  mocks.devServerSnapshot.mockImplementation((terminalId: string) =>
    Promise.resolve(snapshot(terminalId)),
  );
  mocks.startDevServer.mockImplementation((terminalId: string) =>
    Promise.resolve(snapshot(terminalId, "running", "run-new", 2)),
  );
  mocks.stopDevServer.mockImplementation((terminalId: string) =>
    Promise.resolve(snapshot(terminalId, "idle", null, 3)),
  );
  mocks.onDevServerEvent.mockImplementation(
    async (
      terminalId: string,
      handler: (event: DevServerEvent) => void,
    ) => {
      mocks.eventHandlers.set(terminalId, handler);
      return () => mocks.eventHandlers.delete(terminalId);
    },
  );
  usePanels.setState({ devUrl: {}, previewUrl: {} });
  for (const id of [
    "typed",
    "static",
    "static-hydrate",
    "error",
    "running",
    "stale",
  ]) {
    forgetDevState(id);
  }
});

describe("DevTab", () => {
  it("selects a discovered target without exposing arbitrary command input", async () => {
    render(<DevTab terminalId="typed" cwd="/repo" />);

    const select = await screen.findByRole("combobox", { name: "Run target" });
    await waitFor(() => expect((select as HTMLSelectElement).value).toBe("package-script:dev"));
    expect(screen.queryByTitle("The dev-server command to run in this project")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() =>
      expect(mocks.startDevServer).toHaveBeenCalledWith("typed", "/repo", {
        kind: "packageScript",
        script: "dev",
      }),
    );
    expect(await screen.findByText("running")).toBeTruthy();
  });

  it("starts a typed static target and applies its authoritative preview URL", async () => {
    const target = {
      kind: "staticSite" as const,
      id: "static-site:root" as const,
      entrypoint: "index.html" as const,
      relativeRoot: "." as const,
      label: "Static site",
      commandDisplay: "Serve ./index.html",
      recommended: true,
    };
    mocks.discoverRunTargets.mockResolvedValueOnce({
      state: "ready",
      message: null,
      targets: [target],
    });
    mocks.startDevServer.mockResolvedValueOnce({
      ...snapshot("static", "running", "run-static", 2),
      target,
      previewUrl: "http://127.0.0.1:43123/",
    });
    render(<DevTab terminalId="static" cwd="/static" />);

    fireEvent.click(await screen.findByRole("button", { name: "Run" }));

    await waitFor(() =>
      expect(mocks.startDevServer).toHaveBeenCalledWith("static", "/static", {
        kind: "staticSite",
        id: "static-site:root",
      }),
    );
    await waitFor(() =>
      expect(usePanels.getState().devUrl.static).toBe("http://127.0.0.1:43123/"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(usePanels.getState().devUrl.static).toBeNull());
    expect(screen.queryByText("http://127.0.0.1:43123/")).toBeNull();
  });

  it("hydrates the authoritative URL for an already-running static target", async () => {
    const target = {
      kind: "staticSite" as const,
      id: "static-site:root" as const,
      entrypoint: "index.html" as const,
      relativeRoot: "." as const,
      label: "Static site",
      commandDisplay: "Serve ./index.html",
      recommended: true,
    };
    mocks.discoverRunTargets.mockResolvedValueOnce({
      state: "ready",
      message: null,
      targets: [target],
    });
    mocks.devServerSnapshot.mockResolvedValueOnce({
      ...snapshot("static-hydrate", "running", "run-static", 8),
      target,
      previewUrl: "http://127.0.0.1:43124/",
    });
    forgetDevState("static-hydrate");
    render(<DevTab terminalId="static-hydrate" cwd="/static" />);

    await waitFor(() =>
      expect(usePanels.getState().devUrl["static-hydrate"]).toBe(
        "http://127.0.0.1:43124/",
      ),
    );
  });

  it("renders discovery failures with a retry action", async () => {
    mocks.discoverRunTargets.mockResolvedValueOnce({
      state: "invalid",
      targets: [],
      message: "invalid package.json",
    });
    render(<DevTab terminalId="error" cwd="/repo" />);

    expect((await screen.findByRole("alert")).textContent).toContain(
      "invalid package.json",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(mocks.discoverRunTargets).toHaveBeenCalledTimes(2));
  });

  it("hydrates a running backend snapshot and stops only that run", async () => {
    mocks.devServerSnapshot.mockResolvedValue(
      snapshot("running", "running", "run-42", 8),
    );
    render(<DevTab terminalId="running" cwd="/repo" />);

    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    await waitFor(() =>
      expect(mocks.stopDevServer).toHaveBeenCalledWith("running", "run-42"),
    );
  });

  it("ignores output and exits from an older run generation", async () => {
    mocks.devServerSnapshot.mockResolvedValue(
      snapshot("stale", "running", "run-new", 10),
    );
    render(<DevTab terminalId="stale" cwd="/repo" />);
    await screen.findByRole("button", { name: "Stop" });

    mocks.eventHandlers.get("stale")?.({
      id: "stale",
      runId: "run-old",
      revision: 11,
      kind: "line",
      line: "STALE OUTPUT",
    });
    mocks.eventHandlers.get("stale")?.({
      id: "stale",
      runId: "run-old",
      revision: 12,
      kind: "exited",
      line: "old run exited",
    });

    expect(screen.queryByText("STALE OUTPUT")).toBeNull();
    expect(screen.queryByText(/old run exited/)).toBeNull();
    expect(screen.getByText("running")).toBeTruthy();
    expect(usePanels.getState().devUrl.stale).toBeUndefined();
  });
});
