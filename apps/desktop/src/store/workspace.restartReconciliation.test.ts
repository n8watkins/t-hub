// Restart-aligned regression for a retired local Captain designation.
//
// The backend prunes dead tile placements before the webview bootstraps, but the
// frontend also persists presentation-only Captain pins and an orchestrator ID.
// A retired ID that existed in both stores could therefore survive the
// authoritative tab adoption and be sent back in the first full layout report.
import { beforeEach, describe, expect, it } from "vitest";
import { useCaptain } from "./captain";
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

function seed(tabs: WorkspaceTab[]): void {
  useWorkspace.setState({
    tabs,
    activeTabId: "work",
    focusedId: "work-live",
    terminals: {
      "work-live": term("work-live"),
      retired: term("retired"),
      active: term("active"),
    },
    poppedOutTabs: [],
    registryAdopted: false,
  });
  useCaptain.setState({
    captainIds: ["retired"],
    claims: {},
    activeCaptainId: "retired",
    orchestratorId: "retired",
    open: false,
    anchorMenuOpen: false,
  });
}

describe("workspace restart reconciliation", () => {
  beforeEach(() => {
    localStorage.clear();
    seed([
      { id: "work", name: "Workspace", order: ["work-live"] },
      {
        id: CAPTAINS_TAB_ID,
        name: CAPTAINS_TAB_NAME,
        order: ["retired", "active"],
      },
    ]);
  });

  it("does not let a presentation-only retired Captain ID poison the first layout report", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "work", name: "Workspace", kind: "work", tileIds: ["work-live"] },
      {
        id: CAPTAINS_TAB_ID,
        name: CAPTAINS_TAB_NAME,
        kind: "captain",
        tileIds: ["active"],
      },
    ]);

    const state = useWorkspace.getState();
    expect(state.tabs.find((tab) => tab.id === CAPTAINS_TAB_ID)?.order).toEqual([
      "active",
    ]);
    expect(state.terminals.retired).toBeUndefined();
    expect(
      state.tabs.map((tab) => ({ id: tab.id, tileIds: tab.order })),
    ).toEqual([
      { id: "work", tileIds: ["work-live"] },
      { id: CAPTAINS_TAB_ID, tileIds: ["active"] },
    ]);
  });
});
