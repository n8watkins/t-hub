import { beforeEach, describe, expect, it, vi } from "vitest";

const gitWorktreeRemovalPreflight = vi.fn();
const gitWorktreeRemove = vi.fn();

vi.mock("../ipc/git", () => ({
  gitWorktreeRemovalPreflight: (path: string) =>
    gitWorktreeRemovalPreflight(path),
  gitWorktreeRemove: (repo: string, path: string, force?: boolean) =>
    gitWorktreeRemove(repo, path, force),
}));

import { useWorkspace } from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function terminal(id: string, cwd: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, title: id, cwd, state: "live" };
}

beforeEach(() => {
  gitWorktreeRemovalPreflight.mockReset();
  gitWorktreeRemove.mockReset();
  useWorkspace.setState({
    tabs: [{ id: "tab", name: "Worktree", order: ["live"] }],
    terminals: { live: terminal("live", "/repo/worktree/src") },
    activeTabId: "tab",
    focusedId: "live",
  });
});

describe("removeWorktreeWorkspace", () => {
  it("preserves the live tile while authoritative removal safety is unavailable", async () => {
    gitWorktreeRemovalPreflight.mockRejectedValue(
      new Error(
        "worktree removal is temporarily unavailable until the unified worktree status service is available",
      ),
    );

    await expect(
      useWorkspace
        .getState()
        .removeWorktreeWorkspace("/repo", "/repo/worktree", true),
    ).rejects.toThrow("temporarily unavailable");

    expect(gitWorktreeRemovalPreflight).toHaveBeenCalledWith("/repo/worktree");
    expect(gitWorktreeRemove).not.toHaveBeenCalled();
    expect(useWorkspace.getState().tabs[0].order).toEqual(["live"]);
    expect(useWorkspace.getState().terminals.live).toBeDefined();
    expect(useWorkspace.getState().focusedId).toBe("live");
  });
});
