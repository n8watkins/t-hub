import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { commissionCaptain, listProjects, registerProject } from "../ipc/projects";
import { gitInfo, gitWorktreeList } from "../ipc/git";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

vi.mock("../ipc/projects", () => ({
  commissionCaptain: vi.fn(),
  listProjects: vi.fn(),
  registerProject: vi.fn(),
}));
vi.mock("../ipc/git", () => ({
  gitInfo: vi.fn(),
  gitWorktreeList: vi.fn(),
}));
vi.mock("./WslFolderPicker", () => ({
  WslFolderPicker: ({
    path,
    onPathChange,
  }: {
    path: string;
    onPathChange: (path: string) => void;
  }) => (
    <input
      aria-label="Manual WSL path"
      value={path}
      onChange={(event) => onPathChange(event.target.value)}
    />
  ),
}));

describe("CaptainCommissionDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      wslHome: "/home/natkins",
    });
    vi.mocked(gitInfo).mockResolvedValue({
      isRepo: false,
      branch: null,
      worktreeRoot: null,
      isLinkedWorktree: false,
      dirtyCount: 0,
    });
    vi.mocked(gitWorktreeList).mockResolvedValue([]);
    vi.mocked(registerProject).mockResolvedValue({
      projectId: "project-none",
      name: "Appturnity",
      repoRoot: "/home/natkins/appturnity/monorepo-app",
      rootPath: "/home/natkins/appturnity/monorepo-app",
      vcsCapability: "none",
      createdAt: 1,
      updatedAt: 1,
    });
    vi.mocked(commissionCaptain).mockResolvedValue({
      alreadyCommissioned: false,
      captain: { shipSlug: "appturnity", workspaceTabIds: [], crew: [] },
      project: expect.anything(),
      instructions: "",
    } as never);
  });

  it("registers and commissions a populated non-Git folder without Git initialization", async () => {
    const onClose = vi.fn();
    render(<CaptainCommissionDialog open onClose={onClose} onCommissioned={vi.fn()} />);

    fireEvent.change(await screen.findByLabelText("Codebase name"), {
      target: { value: "Appturnity" },
    });
    fireEvent.change(screen.getByLabelText("Manual WSL path"), {
      target: { value: "/home/natkins/appturnity/monorepo-app" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Run the project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));

    await waitFor(() =>
      expect(registerProject).toHaveBeenCalledWith({
        rootPath: "/home/natkins/appturnity/monorepo-app",
        name: "Appturnity",
      }),
    );
    expect(vi.mocked(registerProject).mock.calls[0][0]).not.toHaveProperty("initializeGit");
    await waitFor(() => expect(commissionCaptain).toHaveBeenCalled());
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("requires an explicit non-empty codebase name", async () => {
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Assignment"), {
      target: { value: "Run the project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Codebase name is required");
    expect(registerProject).not.toHaveBeenCalled();
  });
});
