import { beforeEach, describe, expect, it, vi } from "vitest";

const { controlRequest } = vi.hoisted(() => ({ controlRequest: vi.fn() }));

vi.mock("./controlClient", () => ({ controlRequest }));

import {
  captainBootstrap,
  commissionCaptain,
  initializeGit,
  registerProject,
} from "./projects";

describe("Project and Captain bridge contracts", () => {
  beforeEach(() => controlRequest.mockReset());

  it("sends only rootPath and explicit name for non-Git registration", async () => {
    controlRequest.mockResolvedValue({
      projectId: "project-none",
      rootPath: "/home/me/app",
      repoRoot: "/home/me/app",
      name: "App",
      vcsCapability: "none",
    });
    await registerProject({ rootPath: " /home/me/app ", name: " App " });
    expect(controlRequest).toHaveBeenCalledWith("register_project", {
      rootPath: " /home/me/app ",
      name: " App ",
    });
    expect(controlRequest.mock.calls[0][1]).not.toHaveProperty("initializeGit");
  });

  it("keeps explicit Git initialization as a separate bridge operation", async () => {
    controlRequest.mockResolvedValue({ projectId: "project-git" });
    await initializeGit({ rootPath: "/home/me/app", name: "App" });
    expect(controlRequest).toHaveBeenCalledWith("initialize_git", {
      rootPath: "/home/me/app",
      name: "App",
    });
  });

  it("preserves one Project and Captain identity through bootstrap", async () => {
    controlRequest.mockResolvedValue({
      projectId: "project-none",
      rootPath: "/home/me/app",
      repoRoot: "/home/me/app",
      name: "App",
      vcsCapability: "none",
      captain: {
        shipSlug: "app-captain",
        terminalId: "th_cap",
        projectId: "project-none",
        assignment: "Checkpoint",
        workspaceTabIds: [],
        crew: [],
      },
      resumePoint: "checkpoint-1",
      conversationId: "conversation-1",
    });
    const boot = await captainBootstrap({ shipSlug: "app-captain" });
    expect(boot.project.projectId).toBe("project-none");
    expect(boot.project.vcsCapability).toBe("none");
    expect(boot.captain.terminalId).toBe("th_cap");
    expect(controlRequest).toHaveBeenCalledWith("captain_bootstrap", {
      shipSlug: "app-captain",
    });
    await commissionCaptain({
      projectId: boot.project.projectId,
      assignment: "Checkpoint",
      harness: "codex",
    });
    expect(controlRequest).toHaveBeenLastCalledWith("commission_captain", {
      projectId: "project-none",
      assignment: "Checkpoint",
      harness: "codex",
    });
  });

  it("rejects blank display names before any bridge request", async () => {
    await expect(registerProject({ rootPath: "/home/me/app", name: "  " })).rejects.toThrow(
      "non-empty name",
    );
    await expect(initializeGit({ rootPath: "/home/me/app", name: "" })).rejects.toThrow(
      "non-empty name",
    );
    expect(controlRequest).not.toHaveBeenCalled();
  });
});
