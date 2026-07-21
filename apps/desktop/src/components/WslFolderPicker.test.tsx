import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listDir } from "../ipc/files";
import { normalizeWslPath, pickWslFolder } from "../ipc/wslFolderDialog";
import {
  normalizePosixPath,
  parentPath,
  pathBreadcrumbs,
  WslFolderPicker,
} from "./WslFolderPicker";

vi.mock("../ipc/files", () => ({ listDir: vi.fn() }));
vi.mock("../ipc/wslFolderDialog", () => ({
  normalizeWslPath: vi.fn(),
  pickWslFolder: vi.fn(),
}));

describe("WslFolderPicker", () => {
  beforeEach(() => {
    vi.mocked(listDir).mockReset();
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
    vi.mocked(normalizeWslPath).mockImplementation(async (path) => {
      if (path.includes("..")) throw new Error("The WSL folder path cannot contain '..' traversal.");
      if (path.includes("Debian") || /^[A-Za-z]:/.test(path)) {
        throw new Error("Choose a folder inside the configured WSL distribution.");
      }
      return path.trim();
    });
    vi.mocked(pickWslFolder).mockResolvedValue(null);
  });

  it("offers home, recent, breadcrumbs, parent, and manual paths", async () => {
    const onPathChange = vi.fn();
    render(
      <WslFolderPicker
        path="/home/me"
        home="/home/me"
        recentPaths={[{ label: "Recent app", path: "/home/me/app" }]}
        onPathChange={onPathChange}
      />,
    );

    expect(await screen.findByRole("button", { name: "project" })).toBeTruthy();
    expect(screen.queryByText("notes.txt")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "project" }));
    await waitFor(() => expect(onPathChange).toHaveBeenLastCalledWith("/home/me/project"));
    fireEvent.click(screen.getByRole("button", { name: "Recent app" }));
    await waitFor(() => expect(onPathChange).toHaveBeenLastCalledWith("/home/me/app"));
    fireEvent.click(screen.getByRole("button", { name: "Parent folder" }));
    await waitFor(() => expect(onPathChange).toHaveBeenLastCalledWith("/home"));

    fireEvent.change(screen.getByLabelText("Manual WSL path"), {
      target: { value: "/home/me/../other/" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    expect(screen.getByRole("alert").textContent).toContain("traversal");
    await waitFor(() => expect(listDir).toHaveBeenCalledWith("/home/me"));
  });

  it("normalizes POSIX navigation without accepting host paths", () => {
    expect(normalizePosixPath(" /home/me/../app/ ")).toBeNull();
    expect(normalizePosixPath("C:\\Users\\me")).toBeNull();
    expect(parentPath("/home/me/app")).toBe("/home/me");
    expect(parentPath("/")).toBeNull();
    expect(pathBreadcrumbs("/home/me")).toEqual([
      { label: "/", path: "/" },
      { label: "home", path: "/home" },
      { label: "me", path: "/home/me" },
    ]);
  });

  it("keeps folder navigation available without Git probing", async () => {
    render(
      <WslFolderPicker
        path="/home/me"
        recentPaths={[]}
        onPathChange={vi.fn()}
      />,
    );

    expect(await screen.findByRole("button", { name: "project" })).toBeTruthy();
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
    expect(await screen.findByText("Could not list this folder: permission denied")).toBeTruthy();
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

  it("ignores an older listing error after a newer success", async () => {
    let rejectOld!: (cause: Error) => void;
    let resolveNew!: (entries: never[]) => void;
    vi.mocked(listDir)
      .mockImplementationOnce(() => new Promise((_, reject) => { rejectOld = reject; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveNew = resolve; }));
    const view = render(
      <WslFolderPicker path="/home/old" recentPaths={[]} onPathChange={vi.fn()} />,
    );
    view.rerender(<WslFolderPicker path="/home/new" recentPaths={[]} onPathChange={vi.fn()} />);
    rejectOld(new Error("old permission failure"));
    resolveNew([{ name: "new", path: "/home/new/new", isDir: true, isGitRepo: false, size: 0 }] as never[]);
    expect(await screen.findByRole("button", { name: "new" })).toBeTruthy();
    expect(screen.queryByText(/old permission failure/)).toBeNull();
  });

  it("ignores an older normalization success after a newer success", async () => {
    let resolveOld!: (path: string) => void;
    let resolveNew!: (path: string) => void;
    vi.mocked(normalizeWslPath)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveNew = resolve; }));
    const onPathChange = vi.fn();
    render(<WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={onPathChange} />);
    fireEvent.change(screen.getByLabelText("Manual WSL path"), { target: { value: "/home/old" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    fireEvent.change(screen.getByLabelText("Manual WSL path"), { target: { value: "/home/new" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    resolveNew("/home/new");
    await waitFor(() => expect(onPathChange).toHaveBeenCalledWith("/home/new"));
    resolveOld("/home/old");
    await Promise.resolve();
    expect(onPathChange).toHaveBeenCalledTimes(1);
  });

  it("ignores an older normalization error after a newer success", async () => {
    let rejectOld!: (cause: Error) => void;
    let resolveNew!: (path: string) => void;
    vi.mocked(normalizeWslPath)
      .mockImplementationOnce(() => new Promise((_, reject) => { rejectOld = reject; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveNew = resolve; }));
    const onPathChange = vi.fn();
    render(<WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={onPathChange} />);
    fireEvent.change(screen.getByLabelText("Manual WSL path"), { target: { value: "/home/old" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    fireEvent.change(screen.getByLabelText("Manual WSL path"), { target: { value: "/home/new" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    resolveNew("/home/new");
    await waitFor(() => expect(onPathChange).toHaveBeenCalledWith("/home/new"));
    rejectOld(new Error("old normalization failure"));
    await Promise.resolve();
    expect(screen.queryByText("old normalization failure")).toBeNull();
  });

  it("keeps the prior populated result while the newest normalization fails", async () => {
    let rejectNew!: (cause: Error) => void;
    vi.mocked(normalizeWslPath).mockImplementationOnce(
      () => new Promise((_, reject) => { rejectNew = reject; }),
    );
    render(<WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "project" })).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Manual WSL path"), { target: { value: "/home/blocked" } });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    rejectNew(new Error("new normalization failure"));
    expect((await screen.findByRole("alert")).textContent).toContain("new normalization failure");
    expect(screen.getByRole("button", { name: "project" })).toBeTruthy();
    expect(screen.queryByText(/Refreshing this folder listing/)).toBeNull();
  });

  it("shows manual path validation errors without changing the selected folder", async () => {
    const onPathChange = vi.fn();
    render(<WslFolderPicker path="/home/me" recentPaths={[]} onPathChange={onPathChange} />);
    fireEvent.change(screen.getByLabelText("Manual WSL path"), {
      target: { value: "\\\\wsl.localhost\\Debian\\home\\me" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Go" }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(onPathChange).not.toHaveBeenCalled();
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
