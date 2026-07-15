import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  bindProjectPowder,
  commissionCaptain,
  listPowderBoards,
  listProjects,
  registerProject,
} from "../ipc/projects";
import { listDir } from "../ipc/files";
import { gitInfo, gitWorktreeList } from "../ipc/git";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

vi.mock("../ipc/projects", () => ({
  listProjects: vi.fn(),
  listPowderBoards: vi.fn(),
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
    vi.mocked(listPowderBoards).mockReset();
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
    vi.mocked(listPowderBoards).mockResolvedValue({
      connectionProfile: "production",
      boards: [{ name: "t-hub", aliases: [], tier: "active", cardCount: 2 }],
      count: 1,
      totalCount: 1,
      hasMore: false,
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
    expect(screen.getByRole("button", { name: "Create new codebase" })).toBeTruthy();

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

  it("requires an explicit choice when a profile has multiple Powder boards", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
    });
    vi.mocked(listPowderBoards).mockResolvedValue({
      connectionProfile: "production",
      boards: [
        { name: "alpha", aliases: [], tier: "active", cardCount: 3 },
        { name: "t-hub", aliases: [], tier: "active", cardCount: 2 },
      ],
      count: 2,
      totalCount: 2,
      hasMore: false,
    });

    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    const board = await screen.findByLabelText("Powder board");
    await screen.findByRole("option", { name: "alpha (active, 3 cards)" });
    expect((board as HTMLSelectElement).value).toBe("");
    expect(
      (screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    fireEvent.change(board, { target: { value: "t-hub" } });
    expect((board as HTMLSelectElement).value).toBe("t-hub");
  });

  it("reports board discovery failure and retries the selected profile", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
    });
    vi.mocked(listPowderBoards)
      .mockRejectedValueOnce(new Error("Powder HTTP 403: forbidden"))
      .mockResolvedValueOnce({
        connectionProfile: "production",
        boards: [{ name: "t-hub", aliases: [], tier: "active", cardCount: 2 }],
        count: 1,
        totalCount: 1,
        hasMore: false,
      });

    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    expect((await screen.findByRole("alert")).textContent).toContain(
      "Could not load Powder boards",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findByRole("option", { name: "t-hub (active, 2 cards)" });
    expect(listPowderBoards).toHaveBeenCalledTimes(2);
  });

  it("shows a successful empty Powder board catalog", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
    });
    vi.mocked(listPowderBoards).mockResolvedValue({
      connectionProfile: "production",
      boards: [],
      count: 0,
      totalCount: 0,
      hasMore: false,
    });

    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    await screen.findByRole("option", {
      name: "No Powder boards found for this profile",
    });
    expect(
      (screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("creates a reviewed empty codebase at one absent WSL leaf", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
      wslHome: "/home/tester",
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

    fireEvent.click(await screen.findByRole("button", { name: "Create new codebase" }));
    await screen.findByRole("option", { name: "t-hub (active, 2 cards)" });
    fireEvent.change(screen.getByLabelText("New codebase name"), {
      target: { value: "fresh-project" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Build the fresh project" },
    });

    const review = screen.getByRole("region", { name: "Review before creating" });
    expect(within(review).getByText("New empty codebase")).toBeTruthy();
    expect(within(review).getByText("/home/tester/fresh-project")).toBeTruthy();
    expect(within(review).getByText("Create /home/tester/fresh-project")).toBeTruthy();
    expect(screen.getByText("Template and clone starting points will be added later.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

    await waitFor(() => expect(registerProject).toHaveBeenCalledOnce());
    expect(registerProject).toHaveBeenCalledWith({
      repoRoot: "/home/tester/fresh-project",
      name: "fresh-project",
      createDirectory: true,
      initializeGit: true,
      powderRepository: "t-hub",
      powderConnectionProfile: "production",
    });
    expect(commissionCaptain).toHaveBeenCalledWith({
      projectId: "project-1",
      assignment: "Build the fresh project",
      harness: "codex",
    });
  });

  it("refuses an invalid new codebase name before registration", async () => {
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      powderProfiles: ["production"],
      wslHome: "/home/tester",
    });

    render(
      <CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Create new codebase" }));
    await screen.findByRole("option", { name: "t-hub (active, 2 cards)" });
    fireEvent.change(screen.getByLabelText("New codebase name"), {
      target: { value: "nested/project" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Build the fresh project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

    expect(await screen.findByText("Codebase name must be one safe folder name.")).toBeTruthy();
    expect(registerProject).not.toHaveBeenCalled();
    expect(commissionCaptain).not.toHaveBeenCalled();
  });
});
