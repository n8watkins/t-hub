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

  it("round-trips the native explicit-none lifecycle response contract", async () => {
    // The Rust control.rs test `non_git_captain_checkpoint_reload_and_bootstrap_preserve_real_projects`
    // is the real dispatcher/filesystem/tmux gate.
    // This frontend seam consumes the same native command names and response fields at the bridge boundary.
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
      captain_checkpoint: { accepted: "captain_checkpoint" },
      captain_bootstrap: { project, captain, instructions: "", recoverySource: "captains-registry" },
    };
    controlRequest.mockImplementation(async (command: string) => responses[command as keyof typeof responses]);
    const registered = await registerProject({ rootPath: project.rootPath, name: project.name });
    const commissioned = await commissionCaptain({ projectId: registered.projectId, assignment: "Checkpoint", harness: "codex" });
    await captainCheckpoint({ shipSlug: captain.shipSlug, conversationId: captain.conversationId, resumePoint: captain.resumePoint });
    const reloaded = await captainBootstrap({ shipSlug: captain.shipSlug });
    expect(reloaded.project).toMatchObject(project);
    expect(reloaded.captain).toMatchObject(captain);
    expect(commissioned.project).toMatchObject(project);
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
