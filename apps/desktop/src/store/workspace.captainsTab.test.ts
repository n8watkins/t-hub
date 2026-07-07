// The reserved Captains tab polish (captains-tab-polish): two invariants that
// must not be broken by the always-present reserved Captains tab -
//   1. the last WORK tab can never be closed (never park the user on the
//      Captains-only view), and
//   2. a plain WORK spawn never lands in the reserved Captains tab (only
//      captain/orchestrator agent tiles belong there, via moveTileToCaptainsTab).
import { beforeEach, describe, expect, it } from "vitest";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  type WorkspaceTab,
} from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

/** Seed the store, always including the reserved Captains tab (the live store
 *  guarantees it) appended LAST, mirroring the real layout. */
function seed(
  tabs: WorkspaceTab[],
  activeTabId: string,
  focusedId: string | null,
): void {
  const withReserved = tabs.some((t) => t.id === CAPTAINS_TAB_ID)
    ? tabs
    : [...tabs, { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] }];
  const terminals: Record<string, TerminalInfo> = {};
  for (const t of withReserved) for (const id of t.order) terminals[id] = term(id);
  useWorkspace.setState({
    tabs: withReserved,
    activeTabId,
    focusedId,
    terminals,
    poppedOutTabs: [],
    userLabels: {},
    labels: {},
  });
}

const workTabs = () =>
  useWorkspace.getState().tabs.filter((t) => t.id !== CAPTAINS_TAB_ID);
const tab = (id: string) => useWorkspace.getState().tabs.find((t) => t.id === id);

describe("close guards: the last WORK tab is never closeable", () => {
  it("closeTab refuses the last work tab (only the reserved Captains tab would remain)", () => {
    // One work tab + the always-present reserved tab: tabs.length is 2, but the
    // old `tabs.length <= 1` guard would have allowed this close.
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().closeTab("t1");
    expect(tab("t1")).toBeTruthy();
    expect(workTabs().length).toBe(1);
  });

  it("closeTab closes a work tab when another work tab remains", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: "t2", name: "Workspace 2", order: ["b"] },
      ],
      "t1",
      "a",
    );
    useWorkspace.getState().closeTab("t2");
    expect(tab("t2")).toBeUndefined();
    expect(tab("t1")).toBeTruthy();
    expect(tab(CAPTAINS_TAB_ID)).toBeTruthy();
  });

  it("closeTab never closes the reserved Captains tab", () => {
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().closeTab(CAPTAINS_TAB_ID);
    expect(tab(CAPTAINS_TAB_ID)).toBeTruthy();
  });

  it("closeWorkspace refuses the last work tab (mirrors the closeTab guard)", () => {
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().closeWorkspace("t1");
    expect(tab("t1")).toBeTruthy();
  });
});

describe("spawn placement: a work tile never lands in the reserved Captains tab", () => {
  beforeEach(() => {
    // The active tab is the reserved Captains tab - the case that used to
    // misplace a plain spawn.
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], CAPTAINS_TAB_ID, null);
  });

  it("addToTab redirects a tile targeting Captains into a work tab", () => {
    useWorkspace.getState().addToTab(CAPTAINS_TAB_ID, term("new1"));
    expect(tab(CAPTAINS_TAB_ID)?.order).not.toContain("new1");
    expect(tab("t1")?.order).toContain("new1");
    expect(useWorkspace.getState().activeTabId).toBe("t1");
    expect(useWorkspace.getState().focusedId).toBe("new1");
  });

  it("addAfterFocused redirects out of Captains when it is the active tab", () => {
    useWorkspace.getState().addAfterFocused(term("new2"));
    expect(tab(CAPTAINS_TAB_ID)?.order).not.toContain("new2");
    expect(tab("t1")?.order).toContain("new2");
  });

  it("mints a work tab when ONLY the reserved Captains tab exists", () => {
    // All-reserved edge: no work tab present at all.
    seed([], CAPTAINS_TAB_ID, null);
    useWorkspace.getState().addToTab(CAPTAINS_TAB_ID, term("new3"));
    const work = workTabs();
    expect(work.length).toBe(1);
    expect(work[0].order).toContain("new3");
    // The reserved tab stays LAST.
    const all = useWorkspace.getState().tabs;
    expect(all[all.length - 1].id).toBe(CAPTAINS_TAB_ID);
    expect(useWorkspace.getState().activeTabId).toBe(work[0].id);
  });

  it("a normal placement into a real work tab is unaffected", () => {
    useWorkspace.getState().addToTab("t1", term("new4"));
    expect(tab("t1")?.order).toContain("new4");
    expect(tab(CAPTAINS_TAB_ID)?.order).not.toContain("new4");
  });
});
