import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { commissionCaptain, listProjects, registerProject } from "../ipc/projects";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

const pickerSelection = vi.hoisted(() => ({
  current: {
    listingStatus: "valid-populated" as string,
    metadataStatus: "ready" as string,
    git: { isRepo: false } as Record<string, unknown>,
    worktreeCount: 0,
    worktrees: null,
    error: undefined as string | undefined,
  },
}));

vi.mock("../ipc/projects", () => ({
  commissionCaptain: vi.fn(),
  listProjects: vi.fn(),
  registerProject: vi.fn(),
}));
vi.mock("./WslFolderPicker", () => ({
  WslFolderPicker: ({
    path,
    onPathChange,
    onFolderMetadataChange,
    metadataRefreshToken,
  }: {
    path: string;
    onPathChange: (path: string) => void;
    onFolderMetadataChange?: (selection: unknown) => void;
    metadataRefreshToken?: number;
  }) => (
    <input
      aria-label="Manual WSL path"
      data-refresh-token={metadataRefreshToken}
      value={path}
      onChange={(event) => {
        const nextPath = event.target.value;
        onPathChange(nextPath);
        onFolderMetadataChange?.({ ...pickerSelection.current, path: nextPath });
      }}
    />
  ),
}));

describe("CaptainCommissionDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    pickerSelection.current = {
      listingStatus: "valid-populated",
      metadataStatus: "ready",
      git: { isRepo: false },
      worktreeCount: 0,
      worktrees: null,
      error: undefined,
    };
    vi.mocked(listProjects).mockResolvedValue({
      projects: [],
      count: 0,
      seq: 0,
      wslHome: "/home/natkins",
    });
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
    expect(screen.getByText("None")).toBeTruthy();
    await waitFor(() => expect(commissionCaptain).toHaveBeenCalled());
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("shows Git metadata without changing the registration payload", async () => {
    pickerSelection.current = {
      listingStatus: "valid-populated",
      metadataStatus: "ready",
      git: {
        isRepo: true,
        branch: "feature",
        worktreeRoot: "/home/natkins/appturnity/monorepo-app",
        dirtyCount: 2,
        isLinkedWorktree: true,
        remoteUrl: "https://example.test/app.git",
        defaultBranch: "main",
        headCommit: "abc123",
      },
      worktreeCount: 3,
      worktrees: null,
      error: undefined,
    };
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Codebase name"), {
      target: { value: "Appturnity" },
    });
    fireEvent.change(screen.getByLabelText("Manual WSL path"), {
      target: { value: "/home/natkins/appturnity/monorepo-app" },
    });
    expect(screen.getByText("Git")).toBeTruthy();
    expect(screen.getByText("https://example.test/app.git")).toBeTruthy();
    expect(screen.getByText("abc123")).toBeTruthy();
    expect(screen.getByText("3")).toBeTruthy();
    expect(screen.getByText("feature")).toBeTruthy();
    expect(screen.getByText("Linked")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Run the project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));
    await waitFor(() => expect(registerProject).toHaveBeenCalledWith({
      rootPath: "/home/natkins/appturnity/monorepo-app",
      name: "Appturnity",
    }));
  });

  it("uses authoritative rootPath in the saved-codebase review", async () => {
    vi.mocked(listProjects).mockResolvedValueOnce({
      projects: [{
        projectId: "saved-project",
        name: "Canonical App",
        rootPath: "/home/me/canonical-app",
        repoRoot: "/compatibility/wrong-root",
        vcsCapability: "none",
        createdAt: 1,
        updatedAt: 1,
      }],
      count: 1,
      seq: 1,
      wslHome: "/home/me",
    });
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    expect(await screen.findByText("/home/me/canonical-app")).toBeTruthy();
    expect(screen.queryByText("/compatibility/wrong-root")).toBeNull();
    expect(screen.getByText("Canonical App")).toBeTruthy();
  });

  it("shows saved non-Git capability and allows commission preflight", async () => {
    vi.mocked(listProjects).mockResolvedValueOnce({
      projects: [{
        projectId: "saved-none",
        name: "Plain App",
        rootPath: "/home/me/plain-app",
        repoRoot: "/home/me/plain-app",
        vcsCapability: "none",
        createdAt: 1,
        updatedAt: 1,
      }],
      count: 1,
      seq: 1,
      wslHome: "/home/me",
    });
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    expect(await screen.findByText("None")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows saved Git capability and available metadata", async () => {
    vi.mocked(listProjects).mockResolvedValueOnce({
      projects: [{
        projectId: "saved-git",
        name: "Git App",
        rootPath: "/home/me/git-app",
        repoRoot: "/home/me/git-app",
        vcsCapability: "git",
        gitMainRoot: "/home/me/git-app",
        remoteUrl: "https://example.test/git.git",
        defaultBranch: "main",
        createdAt: 1,
        updatedAt: 1,
      }],
      count: 1,
      seq: 1,
      wslHome: "/home/me",
    });
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    expect(await screen.findByText("Git")).toBeTruthy();
    expect(screen.getByText("https://example.test/git.git")).toBeTruthy();
    expect(screen.getAllByText("Unknown").length).toBeGreaterThanOrEqual(5);
    expect(screen.queryByText("Clean")).toBeNull();
    expect(screen.queryByText("Main")).toBeNull();
    expect((screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("blocks existing-folder commission while Version Control is checking", async () => {
    pickerSelection.current = {
      listingStatus: "valid-populated",
      metadataStatus: "checking",
      git: { isRepo: false },
      worktreeCount: 0,
      worktrees: null,
      error: undefined,
    };
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Manual WSL path"), {
      target: { value: "/home/me/checking" },
    });
    expect(screen.getByText("Checking...")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("allows a valid root with unavailable Version Control and exposes retry", async () => {
    pickerSelection.current = {
      listingStatus: "valid-populated",
      metadataStatus: "unavailable",
      git: { isRepo: false },
      worktreeCount: 0,
      worktrees: null,
      error: "permission denied",
    };
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Manual WSL path"), {
      target: { value: "/home/me/blocked" },
    });
    expect(screen.getByText("Unavailable: permission denied")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement).disabled).toBe(false);
    const picker = screen.getByLabelText("Manual WSL path");
    const before = picker.getAttribute("data-refresh-token");
    fireEvent.click(screen.getByRole("button", { name: "Retry Version control check" }));
    expect(picker.getAttribute("data-refresh-token")).not.toBe(before);
  });

  it("blocks an invalid root even when Git metadata is ready", async () => {
    pickerSelection.current = {
      listingStatus: "error",
      metadataStatus: "ready",
      git: { isRepo: true },
      worktreeCount: 1,
      worktrees: [],
      error: "directory missing",
    };
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Manual WSL path"), {
      target: { value: "/home/me/missing" },
    });
    expect(screen.getByText("Unavailable: directory missing")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Create Captain" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps display name separate from the new destination leaf", async () => {
    vi.mocked(registerProject).mockResolvedValueOnce({
      projectId: "new-project",
      name: "Marketing Site",
      rootPath: "/home/natkins/marketing-site",
      repoRoot: "/home/natkins/marketing-site",
      vcsCapability: "none",
      createdAt: 1,
      updatedAt: 1,
    });
    render(<CaptainCommissionDialog open onClose={vi.fn()} onCommissioned={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Create new codebase" }));
    fireEvent.change(await screen.findByLabelText("New codebase name"), {
      target: { value: "Marketing Site" },
    });
    fireEvent.change(screen.getByLabelText("Destination folder name"), {
      target: { value: "marketing-site" },
    });
    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Set up the site" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Captain" }));
    await waitFor(() =>
      expect(registerProject).toHaveBeenCalledWith({
        rootPath: "/home/natkins/marketing-site",
        name: "Marketing Site",
        createDirectory: true,
      }),
    );
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
