import { beforeEach, describe, expect, it, vi } from "vitest";

const { controlRequest, invoke, listen } = vi.hoisted(() => ({
  controlRequest: vi.fn(),
  invoke: vi.fn(),
  listen: vi.fn().mockRejectedValue(new Error("not running in Tauri")),
}));

vi.mock("./controlClient", () => ({
  controlRequest,
  onControlEvent: () => () => {},
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import {
  bootstrapWorkspaceTabs,
  bootstrapWorkspaceTabsUntilReady,
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
      return Promise.resolve([]);
    }
    if (command === "report_workspace_tabs") {
      return Promise.resolve({ seq: 2, stale: false });
    }
    return Promise.reject(new Error(`unexpected invoke: ${command}`));
  });
});

describe("workspace registry bootstrap", () => {
  it("does not start the bridge outside a Tauri webview", () => {
    expect(listen).not.toHaveBeenCalled();
    expect(controlRequest).not.toHaveBeenCalled();
  });

  it("repairs a Captain-only snapshot with only live local terminal IDs", async () => {
    seed([
      {
        id: "work-1",
        name: "Workspace 1",
        order: ["term-live", "term-stale"],
      },
      {
        id: CAPTAINS_TAB_ID,
        name: "Captain Workspace",
        order: ["captain-live", "captain-stale"],
      },
    ]);
    controlRequest.mockResolvedValue({
      seq: 1,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });
    invoke.mockImplementation((command: string) => {
      if (command === "list_terminals") {
        return Promise.resolve([
          { id: "term-live" },
          { id: "captain-live" },
          { id: "unplaced-live" },
        ]);
      }
      if (command === "report_workspace_tabs") {
        return Promise.resolve({ seq: 2, stale: false });
      }
      return Promise.reject(new Error(`unexpected invoke: ${command}`));
    });

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(true);

    expect(
      useWorkspace.getState().tabs.map((tab) => ({
        id: tab.id,
        tileIds: tab.order,
      })),
    ).toEqual([
      { id: "work-1", tileIds: ["term-live"] },
      { id: CAPTAINS_TAB_ID, tileIds: ["captain-live"] },
    ]);
    expect(useWorkspace.getState().registryAdopted).toBe(true);
    expect(invoke).toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.objectContaining({
        baseSeq: 1,
        tabs: [
          expect.objectContaining({ id: "work-1", tileIds: ["term-live"] }),
          expect.objectContaining({
            id: CAPTAINS_TAB_ID,
            tileIds: ["captain-live"],
          }),
        ],
      }),
    );
  });

  it("does not repair the registry when terminal liveness is unavailable", async () => {
    seed([
      { id: "work-1", name: "Workspace 1", order: ["term-stale"] },
      { id: CAPTAINS_TAB_ID, name: "Captain Workspace", order: [] },
    ]);
    controlRequest.mockResolvedValue({
      seq: 1,
      activeTabId: CAPTAINS_TAB_ID,
      tabs: [{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: [] }],
    });
    invoke.mockRejectedValue(new Error("terminal scan unavailable"));

    await expect(bootstrapWorkspaceTabs()).resolves.toBe(false);

    expect(invoke).not.toHaveBeenCalledWith(
      "report_workspace_tabs",
      expect.anything(),
    );
  });

  it("retries an indeterminate bootstrap until the registry is authoritative", async () => {
    seed([{ id: "work-1", name: "Workspace 1", order: ["term-local"] }]);
    controlRequest
      .mockRejectedValueOnce(new Error("control channel unavailable"))
      .mockResolvedValueOnce({
        seq: 3,
        activeTabId: "work-live",
        tabs: [{ id: "work-live", name: "Live Workspace", tileIds: ["term-live"] }],
      });
    const wait = vi.fn().mockResolvedValue(undefined);

    await expect(bootstrapWorkspaceTabsUntilReady(wait)).resolves.toBe(true);

    expect(wait).toHaveBeenCalledWith(1_000);
    expect(controlRequest).toHaveBeenCalledTimes(2);
    expect(
      useWorkspace.getState().tabs.map((tab) => ({
        id: tab.id,
        tileIds: tab.order,
      })),
    ).toEqual([
      { id: "work-live", tileIds: ["term-live"] },
      { id: CAPTAINS_TAB_ID, tileIds: [] },
    ]);
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
});
