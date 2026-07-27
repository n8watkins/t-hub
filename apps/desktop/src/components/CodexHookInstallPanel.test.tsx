import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("../ipc/client05", () => ({
  codexHooksHealth: vi.fn(),
  installCodexHooks: vi.fn(),
  repairCodexHooks: vi.fn(),
  uninstallCodexHooks: vi.fn(),
}));

import {
  codexHooksHealth,
  installCodexHooks,
  repairCodexHooks,
  uninstallCodexHooks,
} from "../ipc/client05";
import type {
  CodexHookStatus,
  CodexHooksHealth,
  CodexHooksInstallReport,
} from "../ipc/model";
import { CodexHookInstallPanel } from "./CodexHookInstallPanel";

const EVENTS = [
  "SessionStart",
  "UserPromptSubmit",
  "PermissionRequest",
  "PreToolUse",
  "PostToolUse",
  "Stop",
  "SessionEnd",
];

function health(
  status: CodexHookStatus,
  overrides: Partial<CodexHooksHealth> = {},
): CodexHooksHealth {
  const installed = status !== "notInstalled";
  return {
    status,
    hooksPath: "/home/test/.codex/hooks.json",
    configPath: "/home/test/.codex/config.toml",
    requirementsPath: "/etc/codex/requirements.toml",
    managedEvents: installed ? EVENTS : [],
    missingEvents: installed ? [] : EVENTS,
    executablePath: "/usr/local/bin/t-hub-agent",
    executableOk: true,
    agentCapable: true,
    agentVersion: "0.5.3",
    hooksEnabled: true,
    inlineUserHooksPresent: false,
    projectHooksPresent: false,
    pluginConfigPresent: false,
    managedHooksPresent: false,
    managedOnlyPolicy: false,
    capabilities: {
      sessionStart: "native_hook",
      userPrompt: "native_hook",
      permission: "native_hook",
      completion: "native_hook",
      sessionEnd: "native_hook",
      question: "native_hook",
      failure: "structured_app_server_or_degraded",
    },
    ...overrides,
  };
}

function report(next: CodexHooksHealth): CodexHooksInstallReport {
  return {
    hooksPath: next.hooksPath,
    changed: true,
    backedUp: false,
    managedEvents: next.managedEvents.length,
    health: next,
  };
}

beforeEach(() => {
  vi.mocked(codexHooksHealth).mockReset();
  vi.mocked(installCodexHooks).mockReset();
  vi.mocked(repairCodexHooks).mockReset();
  vi.mocked(uninstallCodexHooks).mockReset();
});

describe("CodexHookInstallPanel", () => {
  it("explains the required Codex trust review before voice can activate", async () => {
    vi.mocked(codexHooksHealth).mockResolvedValue(health("needsReview"));

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    expect(await screen.findByText("needs review")).not.toBeNull();
    expect(screen.getByText("One Codex review is still required")).not.toBeNull();
    expect(screen.getByText("/hooks")).not.toBeNull();
    expect(screen.getByText(/Voice and lifecycle updates remain inactive/)).not.toBeNull();
    expect(screen.getByText("7/7")).not.toBeNull();
    expect(
      (screen.getByRole("button", {
        name: "Uninstall Codex hooks",
      }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("requires consent to install and adopts the returned health without a real config write", async () => {
    const notInstalled = health("notInstalled");
    const needsReview = health("needsReview");
    vi.mocked(codexHooksHealth).mockResolvedValue(notInstalled);
    vi.mocked(installCodexHooks).mockResolvedValue(report(needsReview));

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    const install = await screen.findByRole("button", { name: "Install Codex hooks" });
    expect((install as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /I consent to editing my global ~\/.codex\/hooks.json/,
      }),
    );
    expect((install as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(install);

    await waitFor(() =>
      expect(installCodexHooks).toHaveBeenCalledWith("t-hub-agent", true),
    );
    expect(await screen.findByText("Codex hook file updated")).not.toBeNull();
    expect(screen.getByText("needs review")).not.toBeNull();
    expect(screen.getByText("One Codex review is still required")).not.toBeNull();
  });

  it("exposes consent-gated repair only for repairable drift", async () => {
    const drifted = health("drifted", {
      managedEvents: EVENTS.slice(0, 4),
      missingEvents: ["SessionEnd"],
    });
    vi.mocked(codexHooksHealth).mockResolvedValue(drifted);
    vi.mocked(repairCodexHooks).mockResolvedValue(report(health("needsReview")));

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    const repair = await screen.findByRole("button", { name: "Repair Codex hooks" });
    expect((repair as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: /I consent/ }));
    fireEvent.click(repair);

    await waitFor(() =>
      expect(repairCodexHooks).toHaveBeenCalledWith("t-hub-agent", true),
    );
  });

  it("uninstalls managed hooks without requiring configuration consent", async () => {
    const healthy = health("healthy");
    const removed = health("notInstalled");
    vi.mocked(codexHooksHealth).mockResolvedValue(healthy);
    vi.mocked(uninstallCodexHooks).mockResolvedValue(report(removed));

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Uninstall Codex hooks" }),
    );

    await waitFor(() =>
      expect(uninstallCodexHooks).toHaveBeenCalledWith("t-hub-agent"),
    );
    expect(await screen.findByText("Codex hooks removed")).not.toBeNull();
    expect(screen.getByText("not installed")).not.toBeNull();
  });

  it("surfaces health errors and leaves a refresh path", async () => {
    vi.mocked(codexHooksHealth).mockRejectedValue(
      new Error("could not inspect WSL Codex home"),
    );

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    expect((await screen.findByRole("alert")).textContent).toContain(
      "could not inspect WSL Codex home",
    );
    expect(
      (screen.getByRole("button", {
        name: "Refresh status",
      }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("blocks repair when the WSL helper is unavailable", async () => {
    vi.mocked(codexHooksHealth).mockResolvedValue(
      health("drifted", { executableOk: false }),
    );

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    expect(await screen.findByText("WSL helper unavailable")).not.toBeNull();
    expect(screen.queryByRole("button", { name: "Repair Codex hooks" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: /I consent/ })).toBeNull();
  });

  it("identifies an executable but outdated WSL helper as unsupported", async () => {
    vi.mocked(codexHooksHealth).mockResolvedValue(
      health("drifted", {
        executableOk: true,
        agentCapable: false,
        agentVersion: "0.5.2",
      }),
    );

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    expect(await screen.findByText("WSL helper needs an update")).not.toBeNull();
    expect(screen.getAllByText(/0.5.2/)).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Repair Codex hooks" })).toBeNull();
  });

  it("reports install failures without claiming the hook file changed", async () => {
    vi.mocked(codexHooksHealth).mockResolvedValue(health("notInstalled"));
    vi.mocked(installCodexHooks).mockRejectedValue(
      new Error("refusing to modify malformed hooks.json"),
    );

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    fireEvent.click(
      await screen.findByRole("checkbox", {
        name: /I consent to editing my global ~\/.codex\/hooks.json/,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Install Codex hooks" }));

    expect(await screen.findByRole("alert")).not.toBeNull();
    expect(screen.getByRole("alert").textContent).toContain(
      "refusing to modify malformed hooks.json",
    );
    expect(screen.queryByText("Codex hook file updated")).toBeNull();
  });

  it("explains when the Codex hooks feature is globally disabled", async () => {
    vi.mocked(codexHooksHealth).mockResolvedValue(
      health("disabled", { hooksEnabled: false }),
    );

    render(<CodexHookInstallPanel agentBin="t-hub-agent" />);

    expect(await screen.findByText("Enable Codex hooks")).not.toBeNull();
    expect(screen.getByText("[features].hooks = true")).not.toBeNull();
    expect(screen.getByText("~/.codex/config.toml")).not.toBeNull();
    expect(
      screen.queryByText("Enable the handlers in Codex"),
    ).toBeNull();
  });
});
