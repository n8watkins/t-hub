import { beforeEach, describe, expect, it, vi } from "vitest";

const { controlRequest } = vi.hoisted(() => ({ controlRequest: vi.fn() }));

vi.mock("./controlClient", () => ({ controlRequest }));

import {
  captainBootstrap,
  captainCheckpoint,
  commissionCaptain,
  initializeGit,
  registerProject,
} from "./projects";

describe("Project and Captain bridge contracts", () => {
  beforeEach(() => controlRequest.mockReset());

  it("sends only rootPath and explicit name for non-Git registration", async () => {
    controlRequest.mockResolvedValue({
      project: {
        projectId: "project-none",
        rootPath: "/home/me/app",
        repoRoot: "/home/me/app",
        name: "App",
        vcsCapability: "none",
      },
      captain: {
        shipSlug: "app-captain",
        terminalId: "th_cap",
        projectId: "project-none",
        assignment: "Checkpoint",
        conversationId: "conversation-1",
        resumePoint: "checkpoint-1",
        workspaceTabIds: [],
        crew: [],
      },
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
      project: {
        projectId: "project-none",
        rootPath: "/home/me/app",
        repoRoot: "/home/me/app",
        name: "App",
        vcsCapability: "none",
      },
      captain: {
        shipSlug: "app-captain",
        terminalId: "th_cap",
        projectId: "project-none",
        assignment: "Checkpoint",
        conversationId: "conversation-1",
        resumePoint: "checkpoint-1",
        workspaceTabIds: [],
        crew: [],
      },
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

  it("covers the frontend IPC contract paired with the real explicit-none Rust lifecycle", async () => {
    // The named Rust test `non_git_captain_checkpoint_reload_and_bootstrap_preserve_real_projects`
    // is the real dispatcher/filesystem/tmux/restart gate for populated and empty roots.
    // This test deliberately covers only the frontend bridge contract and does not claim to run that dispatcher.
    const project = {
      projectId: "project-none-real-dispatch",
      rootPath: "/home/me/non-git",
      repoRoot: "/home/me/non-git",
      name: "Non-Git",
      vcsCapability: "none",
    };
    const captain = {
      shipSlug: "non-git-ship",
      terminalId: "th_cap_native",
      projectId: project.projectId,
      workspaceTabIds: [],
      crew: [],
      conversationId: "conversation-native",
      resumePoint: "resume-native",
    };
    const responses = {
      register_project: project,
      commission_captain: { alreadyCommissioned: false, project, captain, instructions: "" },
      captain_checkpoint: {
        accepted: "captain_checkpoint" as const,
        audited: true,
        captain,
        target: "captain" as const,
      },
      captain_bootstrap: { project, captain, instructions: "", recoverySource: "captains-registry" },
    };
    controlRequest.mockImplementation(async (command: string) => responses[command as keyof typeof responses]);
    const registered = await registerProject({ rootPath: project.rootPath, name: project.name });
    const commissioned = await commissionCaptain({ projectId: registered.projectId, assignment: "Checkpoint", harness: "codex" });
    const checkpoint = await captainCheckpoint({
      shipSlug: captain.shipSlug,
      conversationId: captain.conversationId,
      resumePoint: captain.resumePoint,
    });
    const reloaded = await captainBootstrap({ shipSlug: captain.shipSlug });
    expect(reloaded.project).toMatchObject(project);
    expect(reloaded.captain).toMatchObject(captain);
    expect(commissioned.project).toMatchObject(project);
    expect(checkpoint).toMatchObject({
      accepted: "captain_checkpoint",
      audited: true,
      target: "captain",
      captain: { conversationId: "conversation-native", resumePoint: "resume-native" },
    });
    expect(reloaded.captain.conversationId).toBe("conversation-native");
    expect(reloaded.captain.resumePoint).toBe("resume-native");
    expect(controlRequest).toHaveBeenNthCalledWith(1, "register_project", { rootPath: project.rootPath, name: project.name });
    expect(controlRequest).toHaveBeenNthCalledWith(2, "commission_captain", { projectId: project.projectId, assignment: "Checkpoint", harness: "codex" });
    expect(controlRequest).toHaveBeenNthCalledWith(3, "captain_checkpoint", { shipSlug: captain.shipSlug, conversationId: captain.conversationId, resumePoint: captain.resumePoint });
    expect(controlRequest).toHaveBeenNthCalledWith(4, "captain_bootstrap", { shipSlug: captain.shipSlug });
    expect(project.vcsCapability).toBe("none");
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
