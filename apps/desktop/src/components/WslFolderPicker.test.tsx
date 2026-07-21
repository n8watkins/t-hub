import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listDir } from "../ipc/files";
import { gitInfo } from "../ipc/git";
import { pickWslFolder } from "../ipc/wslFolderDialog";
import {
  normalizePosixPath,
  parentPath,
  pathBreadcrumbs,
  WslFolderPicker,
} from "./WslFolderPicker";

vi.mock("../ipc/files", () => ({ listDir: vi.fn() }));
vi.mock("../ipc/git", () => ({ gitInfo: vi.fn() }));
vi.mock("../ipc/wslFolderDialog", () => ({ pickWslFolder: vi.fn() }));

describe("WslFolderPicker", () => {
  beforeEach(() => {
    vi.mocked(listDir).mockReset();
    vi.mocked(gitInfo).mockReset();
    vi.mocked(listDir).mockResolvedValue([
      {
        name: "project",
        path: "/home/me/project",
        isDir: true,
        isGitRepo: true,
        size: 0,
      },
      {
        name: "notes.txt",
        path: "/home/me/notes.txt",
        isDir: false,
        isGitRepo: false,
        size: 4,
      },
    ]);
    vi.mocked(gitInfo).mockResolvedValue({
      isRepo: false,
      branch: null,
      worktreeRoot: null,
      isLinkedWorktree: false,
      dirtyCount: 0,
    });
    vi.mocked(pickWslFolder).mockResolvedValue(null);
  });

  it("offers home, recent, breadcrumbs, parent, Git markers, and manual paths", async () => {
    const onPathChange = vi.fn();
    render(
      <WslFolderPicker
        path="/home/me"
        home="/home/me"
        recentPaths={[{ label: "Recent app", path: "/home/me/app" }]}
        onPathChange={onPathChange}
      />,
    );

    expect(await screen.findByRole("button", { name: "project Git" })).toBeTruthy();
    expect(screen.getByText("Git")).toBeTruthy();
    expect(screen.queryByText("notes.txt")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "project Git" }));
    expect(onPathChange).toHaveBeenLastCalledWith("/home/me/project");
    fireEvent.click(screen.getByRole("button", { name: "Recent app" }));
    expect(onPathChange).toHaveBeenLastCalledWith("/home/me/app");
    fireEvent.click(screen.getByRole("button", { name: "Parent folder" }));
    expect(onPathChange).toHaveBeenLastCalledWith("/home");

    fireEvent.change(screen.getByLabelText("Manual WSL path"), {
      target: { value: "/home/me/../other/" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    expect(onPathChange).toHaveBeenLastCalledWith("/home/other");
    await waitFor(() => expect(listDir).toHaveBeenCalledWith("/home/me"));
  });

  it("normalizes POSIX navigation without accepting host paths", () => {
    expect(normalizePosixPath(" /home/me/../app/ ")).toBe("/home/app");
    expect(normalizePosixPath("C:\\Users\\me")).toBeNull();
    expect(parentPath("/home/me/app")).toBe("/home/me");
    expect(parentPath("/")).toBeNull();
    expect(pathBreadcrumbs("/home/me")).toEqual([
      { label: "/", path: "/" },
      { label: "home", path: "/home" },
      { label: "me", path: "/home/me" },
    ]);
  });

  it("keeps folder navigation available when Git status fails", async () => {
    vi.mocked(gitInfo).mockRejectedValue(new Error("git unavailable"));
    render(
      <WslFolderPicker
        path="/home/me"
        recentPaths={[]}
        onPathChange={vi.fn()}
      />,
    );

    expect(await screen.findByRole("button", { name: "project Git" })).toBeTruthy();
    expect(screen.queryByText("git unavailable")).toBeNull();
  });

  it("distinguishes a true empty folder from a directory-list failure", async () => {
    vi.mocked(listDir).mockResolvedValueOnce([]);
    const { unmount } = render(
      <WslFolderPicker path="/home/empty" recentPaths={[]} onPathChange={vi.fn()} />,
    );
    expect(await screen.findByText("This folder is empty.")).toBeTruthy();

    unmount();
    vi.mocked(listDir).mockRejectedValueOnce(new Error("permission denied"));
    render(<WslFolderPicker path="/home/blocked" recentPaths={[]} onPathChange={vi.fn()} />);
    expect(await screen.findByText("Could not list this folder.")).toBeTruthy();
    expect(screen.queryByText("This folder is empty.")).toBeNull();
    expect((await screen.findByRole("alert")).textContent).toContain("permission denied");
  });

  it("does not let an older directory response replace the current folder", async () => {
    let resolveOld!: (entries: never[]) => void;
    let resolveNew!: (entries: never[]) => void;
    vi.mocked(listDir)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveNew = resolve; }));
    const view = render(
      <WslFolderPicker path="/home/old" recentPaths={[]} onPathChange={vi.fn()} />,
    );
    view.rerender(<WslFolderPicker path="/home/new" recentPaths={[]} onPathChange={vi.fn()} />);
    resolveOld([{ name: "old", path: "/home/old/old", isDir: true, isGitRepo: false, size: 0 }] as never[]);
    resolveNew([{ name: "new", path: "/home/new/new", isDir: true, isGitRepo: false, size: 0 }] as never[]);
    expect(await screen.findByRole("button", { name: "new" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "old" })).toBeNull();
  });

  it("opens Explorer and adopts only its validated WSL selection", async () => {
    const onPathChange = vi.fn();
    vi.mocked(pickWslFolder).mockResolvedValue("/home/me/project");
    render(
      <WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={onPathChange} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Browse in Explorer" }));
    await waitFor(() => expect(pickWslFolder).toHaveBeenCalledWith("/home/me"));
    expect(onPathChange).toHaveBeenCalledWith("/home/me/project");
  });

  it("keeps the current folder when Explorer is cancelled", async () => {
    const onPathChange = vi.fn();
    render(
      <WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={onPathChange} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Browse in Explorer" }));
    await waitFor(() => expect(pickWslFolder).toHaveBeenCalled());
    expect(onPathChange).not.toHaveBeenCalled();
  });
});
