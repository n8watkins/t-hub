import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listDir } from "../ipc/files";
import { gitInfo } from "../ipc/git";
import {
  normalizePosixPath,
  parentPath,
  pathBreadcrumbs,
  WslFolderPicker,
} from "./WslFolderPicker";

vi.mock("../ipc/files", () => ({ listDir: vi.fn() }));
vi.mock("../ipc/git", () => ({ gitInfo: vi.fn() }));

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
});
