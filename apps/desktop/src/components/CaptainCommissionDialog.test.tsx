import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  bindProjectPowder,
  commissionCaptain,
  listProjects,
  registerProject,
} from "../ipc/projects";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

vi.mock("../ipc/projects", () => ({
  listProjects: vi.fn(),
  registerProject: vi.fn(),
  bindProjectPowder: vi.fn(),
  commissionCaptain: vi.fn(),
}));

const project = {
  projectId: "project-1",
  name: "T-Hub",
  repoRoot: "/repo/t-hub",
  powder: { connectionProfile: "production", repository: "t-hub" },
  createdAt: 1,
  updatedAt: 1,
};

describe("CaptainCommissionDialog", () => {
  beforeEach(() => {
    vi.mocked(listProjects).mockReset();
    vi.mocked(registerProject).mockReset();
    vi.mocked(bindProjectPowder).mockReset();
    vi.mocked(commissionCaptain).mockReset();
  });

  it("registers an existing repository with Powder before commissioning", async () => {
    vi.mocked(listProjects).mockResolvedValue({ projects: [], count: 0, seq: 0 });
    vi.mocked(registerProject).mockResolvedValue(project);
    vi.mocked(commissionCaptain).mockResolvedValue({
      captain: {
        shipSlug: "t-hub",
        projectId: "project-1",
        workspaceTabIds: [],
        crew: [],
      },
      project,
      instructions: "recover",
      alreadyCommissioned: false,
    });
    const onClose = vi.fn();
    const onCommissioned = vi.fn();
    render(
      <CaptainCommissionDialog
        open
        onClose={onClose}
        onCommissioned={onCommissioned}
      />,
    );

    fireEvent.change(await screen.findByLabelText("Repository path"), {
      target: { value: "/repo/t-hub" },
    });
    fireEvent.change(screen.getByLabelText("Powder repository"), {
      target: { value: "t-hub" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Own production stability" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Commission Captain" }));

    await waitFor(() => expect(registerProject).toHaveBeenCalledOnce());
    expect(registerProject).toHaveBeenCalledWith({
      repoRoot: "/repo/t-hub",
      name: undefined,
      powderRepository: "t-hub",
      powderConnectionProfile: "production",
    });
    expect(commissionCaptain).toHaveBeenCalledWith({
      projectId: "project-1",
      assignment: "Own production stability",
      harness: "codex",
    });
    expect(onCommissioned).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("commissions a registered Powder-bound project without rebinding it", async () => {
    vi.mocked(listProjects).mockResolvedValue({ projects: [project], count: 1, seq: 1 });
    vi.mocked(commissionCaptain).mockResolvedValue({
      captain: {
        shipSlug: "t-hub",
        projectId: "project-1",
        workspaceTabIds: [],
        crew: [],
      },
      project,
      instructions: "recover",
      alreadyCommissioned: false,
    });
    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    await screen.findByRole("option", { name: "T-Hub" });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Own releases" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^claude$/i }));
    fireEvent.click(screen.getByRole("button", { name: "Commission Captain" }));

    await waitFor(() => expect(commissionCaptain).toHaveBeenCalledOnce());
    expect(bindProjectPowder).not.toHaveBeenCalled();
    expect(commissionCaptain).toHaveBeenCalledWith({
      projectId: "project-1",
      assignment: "Own releases",
      harness: "claude",
    });
  });
});
