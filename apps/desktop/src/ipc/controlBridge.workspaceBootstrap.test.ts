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

import {
  bootstrapWorkspaceTabs,
  rebaseStartupWorkspaceTabs,
} from "./controlBridge";
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
    if (command === "list_terminals") {
      return Promise.resolve(
        useWorkspace
          .getState()
          .tabs.flatMap((tab) => tab.order)
          .map((id) => ({ id })),
      );
    }
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

  it("filters dead local terminal ids before repairing a Captain-only registry", async () => {
    seed([
      { id: "work-1", name: "Workspace 1", order: ["term-live", "term-dead"] },
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
    controlRequest.mockResolvedValue({
      seq: 1,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });
    invoke.mockImplementation((command: string, args: unknown) => {
      if (command === "list_terminals") {
        return Promise.resolve([{ id: "term-live" }]);
      }
      if (command === "report_workspace_tabs") {
        return Promise.resolve({ seq: 2, stale: false, args });
      }
      return Promise.reject(new Error(`unexpected invoke: ${command}`));
    });

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(true);

    expect(invoke).toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.objectContaining({
        tabs: expect.arrayContaining([
          expect.objectContaining({ id: "work-1", tileIds: ["term-live"] }),
        ]),
      }),
    );
    expect(
      useWorkspace.getState().tabs.find((tab) => tab.id === "work-1")?.order,
    ).toEqual(["term-live"]);
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

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(true);

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

  it("keeps a durable Cortana and work layout through an empty cold-boot terminal scan", async () => {
    const workTileIds = [
      "253a60dc",
      "4464d15d",
      "7f433705",
      "99246c2f",
      "9f5092dd",
      "c84bfc45",
      "d8170451",
    ];
    seed([
      { id: "work-1", name: "Workspace 1", order: workTileIds },
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
    controlRequest.mockResolvedValue({
      seq: 1,
      activeTabId: "work-1",
      tabs: [
        { id: "work-1", name: "Workspace 1", kind: "work", tileIds: workTileIds },
        {
          id: CAPTAINS_TAB_ID,
          name: "Captain Workspace",
          kind: "captain",
          tileIds: ["5bfa4f12"],
        },
      ],
    });

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(true);
    useWorkspace.getState().setTerminals([]);

    const state = useWorkspace.getState();
    expect(state.tabs.find((tab) => tab.id === "work-1")?.order).toEqual(workTileIds);
    expect(state.tabs.find((tab) => tab.id === CAPTAINS_TAB_ID)?.order).toEqual([
      "5bfa4f12",
    ]);
    const nextReport = state.tabs.map((tab) => ({ id: tab.id, tileIds: tab.order }));
    expect(nextReport).toEqual([
      { id: "work-1", tileIds: workTileIds },
      { id: CAPTAINS_TAB_ID, tileIds: ["5bfa4f12"] },
    ]);

    useWorkspace.getState().adoptRegistry([
      { id: "work-1", name: "Workspace 1", kind: "work", tileIds: [] },
      {
        id: CAPTAINS_TAB_ID,
        name: "Captain Workspace",
        kind: "captain",
        tileIds: [],
      },
    ]);
    expect(useWorkspace.getState().tabs.map((tab) => tab.order)).toEqual([[], []]);
  });

  it("seeds a work workspace when both sides are Captain-only", async () => {
    seed([{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] }]);
    controlRequest.mockResolvedValue({
      seq: 7,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(true);

    const tabs = useWorkspace.getState().tabs;
    expect(tabs.map((tab) => tab.id)).toHaveLength(2);
    expect(tabs.some((tab) => tab.id !== CAPTAINS_TAB_ID)).toBe(true);
    expect(useWorkspace.getState().activeTabId).not.toBe(CAPTAINS_TAB_ID);
    expect(invoke).toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.objectContaining({ baseSeq: 7 }),
    );
  });

  it("keeps the local work workspace when the native report returns an error", async () => {
    seed([{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] }]);
    controlRequest.mockResolvedValue({
      seq: 8,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });
    invoke.mockResolvedValue({
      seq: 8,
      stale: true,
      error: "Workspace report rejected",
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(false);

    expect(useWorkspace.getState().tabs.some((tab) => tab.id !== CAPTAINS_TAB_ID)).toBe(true);
    expect(useWorkspace.getState().tabs).not.toEqual([
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
  });

  it("rebases local workspace changes before adopting the startup registry", async () => {
    seed([
      { id: "work-1", name: "Workspace 1", order: ["term-existing"] },
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
    let resolveControl: ((value: unknown) => void) | undefined;
    controlRequest.mockReturnValue(
      new Promise((resolve) => {
        resolveControl = resolve;
      }),
    );
    const baseline = [
      {
        id: "work-1",
        name: "Workspace 1",
        kind: "work" as const,
        tileIds: ["term-existing"],
      },
      {
        id: CAPTAINS_TAB_ID,
        name: "Captain Workspace",
        kind: "captain" as const,
        tileIds: [],
      },
    ];
    const bootstrap = bootstrapWorkspaceTabs((tabs) => {
      const local = useWorkspace.getState().tabs.map((tab) => ({
        id: tab.id,
        name: tab.name,
        kind: tab.kind,
        tileIds: tab.order,
      }));
      return rebaseStartupWorkspaceTabs(tabs, baseline, local);
    });

    useWorkspace.getState().renameTab("work-1", "Renamed locally");
    const addedTabId = useWorkspace.getState().addTab();
    useWorkspace.getState().addToTab(addedTabId, {
      id: "term-new",
      tmuxSession: "th_term-new",
      title: "New terminal",
      cwd: "/tmp/project",
      state: "live",
    });
    resolveControl?.({
      seq: 4,
      activeTabId: "work-1",
      tabs: [
        {
          id: "work-1",
          name: "Renamed remotely",
          kind: "work",
          tileIds: ["term-existing", "term-remote"],
        },
        { id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] },
      ],
    });

    await expect(bootstrap).resolves.toBe(true);

    const state = useWorkspace.getState();
    expect(state.tabs.find((tab) => tab.id === "work-1")).toMatchObject({
      name: "Renamed locally",
      order: ["term-existing", "term-remote"],
    });
    expect(state.tabs.find((tab) => tab.id === addedTabId)?.order).toEqual([
      "term-new",
    ]);
    expect(state.terminals["term-new"]).toMatchObject({ title: "New terminal" });
    expect(state.activeTabId).toBe(addedTabId);
  });
});
