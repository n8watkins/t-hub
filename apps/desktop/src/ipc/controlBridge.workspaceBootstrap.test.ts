import { beforeEach, describe, expect, it, vi } from "vitest";

const { controlRequest, invoke } = vi.hoisted(() => ({
  controlRequest: vi.fn(),
  invoke: vi.fn(),
}));

vi.mock("./controlClient", () => ({
  controlRequest,
  onControlEvent: () => () => {},
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockRejectedValue(new Error("not running in Tauri")),
}));

import { bootstrapWorkspaceTabs } from "./controlBridge";
import {
  CAPTAINS_TAB_ID,
  useWorkspace,
  type WorkspaceTab,
} from "../store/workspace";

function seed(tabs: WorkspaceTab[]): void {
  useWorkspace.setState({
    tabs,
    activeTabId: tabs[0].id,
    focusedId: tabs[0].order[0] ?? null,
    terminals: {},
    poppedOutTabs: [],
    registryAdopted: false,
  });
}

beforeEach(() => {
  controlRequest.mockReset();
  invoke.mockReset();
  invoke.mockImplementation((command: string) => {
    if (command === "report_workspace_tabs") {
      return Promise.resolve({ seq: 2, stale: false });
    }
    return Promise.reject(new Error(`unexpected invoke: ${command}`));
  });
});

describe("workspace registry bootstrap", () => {
  it("repairs a Captain-only server snapshot from the local work layout", async () => {
    seed([
      { id: "work-1", name: "Workspace 1", order: ["term-1"] },
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
    controlRequest.mockResolvedValue({
      seq: 1,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });

    await bootstrapWorkspaceTabs();

    expect(useWorkspace.getState().tabs.map((tab) => tab.id)).toEqual([
      "work-1",
      CAPTAINS_TAB_ID,
    ]);
    expect(invoke).toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.objectContaining({ baseSeq: 1 }),
    );
  });

  it("adopts an existing server work layout before reporting", async () => {
    seed([{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] }]);
    controlRequest.mockResolvedValue({
      seq: 4,
      activeTabId: "work-2",
      tabs: [
        { id: "work-2", name: "Workspace 2", tileIds: ["term-2"] },
        { id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] },
      ],
    });

    await bootstrapWorkspaceTabs();

    expect(useWorkspace.getState().tabs.map((tab) => tab.id)).toEqual([
      "work-2",
      CAPTAINS_TAB_ID,
    ]);
    expect(useWorkspace.getState().registryAdopted).toBe(true);
    expect(invoke).not.toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.anything(),
    );
  });

  it("seeds a work workspace when both sides are Captain-only", async () => {
    seed([{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] }]);
    controlRequest.mockResolvedValue({
      seq: 7,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });

    await bootstrapWorkspaceTabs();

    const tabs = useWorkspace.getState().tabs;
    expect(tabs.map((tab) => tab.id)).toHaveLength(2);
    expect(tabs.some((tab) => tab.id !== CAPTAINS_TAB_ID)).toBe(true);
    expect(useWorkspace.getState().activeTabId).not.toBe(CAPTAINS_TAB_ID);
    expect(invoke).toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.objectContaining({ baseSeq: 7 }),
    );
  });
});
