// Headless-org: the control bridge's apply path. Every organization forward
// carries the authoritative registry snapshot under `args.sync`; these tests pin
// that applying it never steals the user's view, and that server-spawned
// terminals (spawn_terminal / add_worktree_workspace with an id) are ADOPTED,
// not re-spawned.
import { beforeEach, describe, expect, it } from "vitest";
import { applyControl } from "./controlBridge";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  type WorkspaceTab,
} from "../store/workspace";
import type { TerminalInfo } from "./types";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

function seed(tabs: WorkspaceTab[], activeTabId: string, focusedId: string | null): void {
  const terminals: Record<string, TerminalInfo> = {};
  for (const t of tabs) for (const id of t.order) terminals[id] = term(id);
  useWorkspace.setState({
    tabs,
    activeTabId,
    focusedId,
    terminals,
    poppedOutTabs: [],
  });
}

function sync(tabs: { id: string; name: string; tileIds: string[] }[], seq = 7) {
  return { seq, activeTabId: null, tabs };
}

beforeEach(() => {
  seed(
    [
      { id: "t1", name: "Workspace 1", order: ["a"] },
      { id: "t2", name: "hidden", order: [] },
    ],
    "t1",
    "a",
  );
});

describe("applyControl (headless-org forwards)", () => {
  it("move_tile with a snapshot applies into a hidden tab, view untouched", () => {
    applyControl("move_tile", {
      terminalId: "a",
      tabId: "t2",
      sync: sync([
        { id: "t1", name: "Workspace 1", tileIds: [] },
        { id: "t2", name: "hidden", tileIds: ["a"] },
      ]),
    });
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === "t2")?.order).toEqual(["a"]);
    expect(s.activeTabId).toBe("t1");
  });

  it("spawn_terminal with a server id adopts the terminal without focus steal", () => {
    applyControl("spawn_terminal", {
      id: "srv1",
      tmuxSession: "th_srv1",
      cwd: "/tmp/proj",
      name: "logs",
      tabId: "t2",
      sync: sync([
        { id: "t1", name: "Workspace 1", tileIds: ["a"] },
        { id: "t2", name: "hidden", tileIds: ["srv1"] },
      ]),
    });
    const s = useWorkspace.getState();
    expect(s.terminals["srv1"]?.cwd).toBe("/tmp/proj");
    expect(s.terminals["srv1"]?.title).toBe("logs");
    expect(s.tabs.find((t) => t.id === "t2")?.order).toEqual(["srv1"]);
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedId).toBe("a");
  });

  it("new_tab with a snapshot creates the tab in the background (no activation)", () => {
    applyControl("new_tab", {
      id: "t3",
      name: "staging",
      sync: sync([
        { id: "t1", name: "Workspace 1", tileIds: ["a"] },
        { id: "t2", name: "hidden", tileIds: [] },
        { id: "t3", name: "staging", tileIds: [] },
      ]),
    });
    const s = useWorkspace.getState();
    // The reserved Captains tab is always re-appended by adoptRegistry.
    expect(s.tabs.map((t) => t.id)).toEqual([
      "t1",
      "t2",
      "t3",
      CAPTAINS_TAB_ID,
    ]);
    expect(s.activeTabId).toBe("t1");
  });

  it("sync_tabs (close_terminal) removes the dead tile from its hidden tab", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: "t2", name: "hidden", order: ["dead"] },
      ],
      "t1",
      "a",
    );
    applyControl("sync_tabs", {
      sync: sync([
        { id: "t1", name: "Workspace 1", tileIds: ["a"] },
        { id: "t2", name: "hidden", tileIds: [] },
      ]),
    });
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === "t2")?.order).toEqual([]);
    expect(s.terminals["dead"]).toBeUndefined();
  });

  it("does not erase the work layout when a legacy Captain-only sync arrives", () => {
    applyControl("sync_tabs", {
      sync: sync([{ id: CAPTAINS_TAB_ID, name: "Captain Workspace", tileIds: ["a"] }]),
    });
    const s = useWorkspace.getState();
    expect(s.tabs.some((tab) => tab.id === "t1")).toBe(true);
    expect(s.tabs.find((tab) => tab.id === "t1")?.order).toEqual(["a"]);
    expect(s.activeTabId).toBe("t1");
  });

  it("close_tab removes the emptied tab, view untouched when it was hidden", () => {
    applyControl("close_tab", {
      tabId: "t2",
      sync: sync([{ id: "t1", name: "Workspace 1", tileIds: ["a"] }]),
    });
    const s = useWorkspace.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", CAPTAINS_TAB_ID]);
    expect(s.activeTabId).toBe("t1");
  });

  it("add_worktree_workspace with a server terminalId adopts tile + tab headlessly", () => {
    applyControl("add_worktree_workspace", {
      worktreePath: "/repo/wt/feature-x",
      repoRoot: "/repo",
      branch: "feature-x",
      tabId: "t3",
      tabName: "feature-x",
      terminalId: "wt1",
      alreadyCreated: true,
      sync: sync([
        { id: "t1", name: "Workspace 1", tileIds: ["a"] },
        { id: "t2", name: "hidden", tileIds: [] },
        { id: "t3", name: "feature-x", tileIds: ["wt1"] },
      ]),
    });
    const s = useWorkspace.getState();
    expect(s.terminals["wt1"]?.cwd).toBe("/repo/wt/feature-x");
    expect(s.tabs.find((t) => t.id === "t3")?.order).toEqual(["wt1"]);
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedId).toBe("a");
  });

  it("focus_tab is the explicit view switch and still works", () => {
    applyControl("focus_tab", { tabId: "t2" });
    expect(useWorkspace.getState().activeTabId).toBe("t2");
  });

  it("a malformed forward is a safe no-op", () => {
    const before = useWorkspace.getState().tabs;
    applyControl("move_tile", { terminalId: "a" }); // no tabId, no sync
    applyControl("definitely_not_a_command", { x: 1 });
    expect(useWorkspace.getState().tabs).toBe(before);
  });
});
