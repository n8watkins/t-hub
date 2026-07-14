import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  bindProjectPowder,
  commissionCaptain,
  listProjects,
  registerProject,
} from "../ipc/projects";
import { listDir } from "../ipc/files";
import { gitInfo, gitWorktreeList } from "../ipc/git";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

vi.mock("../ipc/projects", () => ({
  listProjects: vi.fn(),
  registerProject: vi.fn(),
  bindProjectPowder: vi.fn(),
  commissionCaptain: vi.fn(),
}));
vi.mock("../ipc/files", () => ({ listDir: vi.fn() }));
vi.mock("../ipc/git", () => ({ gitInfo: vi.fn(), gitWorktreeList: vi.fn() }));

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
    vi.mocked(gitWorktreeList).mockResolvedValue([]);
  });

  it("saves an existing codebase with Powder before creating its Captain", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
    });
    vi.mocked(registerProject).mockResolvedValue(project);
    vi.mocked(gitInfo).mockResolvedValue({
      isRepo: true,
      branch: "main",
      worktreeRoot: "/repo/t-hub",
      isLinkedWorktree: false,
      dirtyCount: 2,
      headCommit: "0123456789abcdef",
      remoteUrl: "https://example.test/t-hub.git",
      defaultBranch: "main",
    });
    vi.mocked(gitWorktreeList).mockResolvedValue([
      { path: "/repo/t-hub", branch: "main", isLinked: false },
      { path: "/repo/t-hub-feature", branch: "feature", isLinked: true },
    ]);
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
    expect(screen.getByText("Unrestricted")).toBeTruthy();
    expect(await screen.findByText("main · 2 changed · main worktree")).toBeTruthy();
    expect(screen.getByText("https://example.test/t-hub.git")).toBeTruthy();
    expect(screen.getByText("2 detected")).toBeTruthy();
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

  it("requires explicit Git initialization for a non-repository folder", async () => {
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
    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    const path = await screen.findByLabelText("Manual WSL path");
    fireEvent.change(path, { target: { value: "/repo/empty" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    fireEvent.change(screen.getByLabelText("Powder board"), {
      target: { value: "t-hub" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Build the new codebase" },
    });

    const initialize = await screen.findByRole("checkbox", {
      name: "Initialize Git repository",
    });
    expect(screen.getByText("Not a Git repository - initialization not authorized")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));
    expect((await screen.findByRole("alert")).textContent).toContain(
      "Initialize Git explicitly",
    );
    expect(registerProject).not.toHaveBeenCalled();

    fireEvent.click(initialize);
    expect(screen.getByText("Initialize with main as the default branch")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

    await waitFor(() => expect(registerProject).toHaveBeenCalledOnce());
    expect(registerProject).toHaveBeenCalledWith({
      repoRoot: "/repo/empty",
      name: undefined,
      initializeGit: true,
      powderRepository: "t-hub",
      powderConnectionProfile: "production",
    });
  });
});
