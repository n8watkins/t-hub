import { beforeEach, describe, expect, it } from "vitest";
import {
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  useWorkspace,
  type WorkspaceTab,
} from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string): TerminalInfo {
  return {
    id,
    tmuxSession: `th_${id}`,
    cwd: "/tmp",
    title: id,
    state: "live",
  };
}

function seed(tabs: WorkspaceTab[], activeTabId: string): void {
  useWorkspace.setState({
    tabs: [
      ...tabs,
      {
        id: CAPTAINS_TAB_ID,
        name: CAPTAINS_TAB_NAME,
        kind: "captain",
        order: [],
      },
    ],
    activeTabId,
    focusedId: tabs.find((tab) => tab.id === activeTabId)?.order[0] ?? null,
    terminals: Object.fromEntries(
      tabs.flatMap((tab) => tab.order.map((id) => [id, term(id)])),
    ),
    poppedOutTabs: [],
  });
}

describe("closeEmptyWorkspaces", () => {
  beforeEach(() => localStorage.clear());

  it("closes every empty workspace while preserving live and reserved workspaces", () => {
    seed(
      [
        { id: "live", name: "Live", order: ["term-1"] },
        { id: "empty-1", name: "Empty 1", order: [] },
        { id: "empty-2", name: "Empty 2", order: [] },
      ],
      "live",
    );

    expect(useWorkspace.getState().closeEmptyWorkspaces()).toEqual([
      "empty-1",
      "empty-2",
    ]);
    expect(useWorkspace.getState().tabs.map((tab) => tab.id)).toEqual([
      "live",
      CAPTAINS_TAB_ID,
    ]);
    expect(useWorkspace.getState().terminals["term-1"]).toBeDefined();
  });

  it("keeps the active workspace when every work workspace is empty", () => {
    seed(
      [
        { id: "empty-1", name: "Empty 1", order: [] },
        { id: "empty-2", name: "Empty 2", order: [] },
        { id: "empty-3", name: "Empty 3", order: [] },
      ],
      "empty-2",
    );

    expect(useWorkspace.getState().closeEmptyWorkspaces()).toEqual([
      "empty-1",
      "empty-3",
    ]);
    expect(useWorkspace.getState().tabs.map((tab) => tab.id)).toEqual([
      "empty-2",
      CAPTAINS_TAB_ID,
    ]);
    expect(useWorkspace.getState().activeTabId).toBe("empty-2");
  });

  it("does nothing when the only work workspace is empty", () => {
    seed([{ id: "only", name: "Only", order: [] }], "only");

    expect(useWorkspace.getState().closeEmptyWorkspaces()).toEqual([]);
    expect(useWorkspace.getState().tabs.map((tab) => tab.id)).toEqual([
      "only",
      CAPTAINS_TAB_ID,
    ]);
  });
});
