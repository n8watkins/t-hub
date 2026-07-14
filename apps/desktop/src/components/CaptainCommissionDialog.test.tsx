import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  bindProjectPowder,
  commissionCaptain,
  listProjects,
  registerProject,
} from "../ipc/projects";
import { listDir } from "../ipc/files";
import { gitInfo } from "../ipc/git";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

vi.mock("../ipc/projects", () => ({
  listProjects: vi.fn(),
  registerProject: vi.fn(),
  bindProjectPowder: vi.fn(),
  commissionCaptain: vi.fn(),
}));
vi.mock("../ipc/files", () => ({ listDir: vi.fn() }));
vi.mock("../ipc/git", () => ({ gitInfo: vi.fn() }));

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
    vi.mocked(listDir).mockResolvedValue([]);
    vi.mocked(gitInfo).mockResolvedValue({
      isRepo: false,
      branch: null,
      worktreeRoot: null,
      isLinkedWorktree: false,
      dirtyCount: 0,
    });
  });

  it("saves an existing codebase with Powder before creating its Captain", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
    });
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

    const wslFolder = await screen.findByLabelText("Manual WSL path");
    expect(screen.getByRole("dialog", { name: "Create Captain" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Use saved codebase" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose existing WSL folder" })).toBeTruthy();

    fireEvent.change(wslFolder, {
      target: { value: "/repo/t-hub" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    fireEvent.change(screen.getByLabelText("Powder board"), {
      target: { value: "t-hub" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Own production stability" },
    });
    expect(screen.getByText("Review before creating")).toBeTruthy();
    expect(screen.getByText("Existing WSL codebase")).toBeTruthy();
    expect(screen.getByText("Harness default")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

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

  it("creates a Captain for a saved Powder-bound codebase without rebinding it", async () => {
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
    const review = screen.getByRole("region", { name: "Review before creating" });
    expect(within(review).getByText("Saved codebase")).toBeTruthy();
    expect(within(review).getByText("t-hub via production")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

    await waitFor(() => expect(commissionCaptain).toHaveBeenCalledOnce());
    expect(bindProjectPowder).not.toHaveBeenCalled();
    expect(commissionCaptain).toHaveBeenCalledWith({
      projectId: "project-1",
      assignment: "Own releases",
      harness: "claude",
    });
  });
});
