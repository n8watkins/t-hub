// Headless-org: the store's server-registry adopt path. The SERVER owns the
// tab/tile organization; `adoptRegistry` merges its snapshot into the store —
// these tests pin the no-focus-steal and lifecycle-cleanup semantics.
import { beforeEach, describe, expect, it } from "vitest";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  type WorkspaceTab,
} from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

/** Seed the store, always including the reserved Captains tab (the live store
 *  guarantees it via finalizeLayout) so adoptRegistry's re-injection is a no-op
 *  on an already-present reserved tab. */
function seed(tabs: WorkspaceTab[], activeTabId: string, focusedId: string | null): void {
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
  });
}

describe("adoptRegistry (server-authoritative snapshot)", () => {
  beforeEach(() => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a", "b"] },
        { id: "t2", name: "hidden", order: [] },
      ],
      "t1",
      "a",
    );
  });

  it("places a tile into a hidden tab without switching the active tab or focus", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "hidden", tileIds: ["c"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === "t2")?.order).toEqual(["c"]);
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedId).toBe("a");
  });

  it("creates a tab that only exists in the registry, still without stealing the view", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "hidden", tileIds: [] },
      { id: "t3", name: "staging", tileIds: ["z"] },
    ]);
    const s = useWorkspace.getState();
    // The reserved Captains tab is always re-appended last by adoptRegistry.
    expect(s.tabs.map((t) => t.id)).toEqual([
      "t1",
      "t2",
      "t3",
      CAPTAINS_TAB_ID,
    ]);
    expect(s.tabs[2].name).toBe("staging");
    expect(s.activeTabId).toBe("t1");
  });

  it("moves a tile out of the active tab and hands focus to a neighbor", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["b"] },
      { id: "t2", name: "hidden", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedId).toBe("b");
    expect(s.terminals["a"]).toBeDefined(); // moved, not closed
  });

  it("drops a closed tab, moving the active tab only when it was the one closed", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: "t2", name: "hidden", order: [] },
      ],
      "t2",
      null,
    );
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", CAPTAINS_TAB_ID]);
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedId).toBe("a");
  });

  it("cleans up tiles that vanished from every tab (closed headlessly)", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
      { id: "t2", name: "hidden", tileIds: [] },
    ]);
    const s = useWorkspace.getState();
    expect(s.terminals["b"]).toBeUndefined();
    expect(s.terminals["a"]).toBeDefined();
  });

  it("is a no-op (same tabs identity) for a deep-equal snapshot", () => {
    const before = useWorkspace.getState().tabs;
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "hidden", tileIds: [] },
    ]);
    expect(useWorkspace.getState().tabs).toBe(before);
  });

  it("ignores an empty snapshot (defensive)", () => {
    useWorkspace.getState().adoptRegistry([]);
    // t1, t2, and the reserved Captains tab.
    expect(useWorkspace.getState().tabs).toHaveLength(3);
  });

  it("applies a rename from the registry", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "renamed-ops", tileIds: [] },
    ]);
    expect(useWorkspace.getState().tabs[1].name).toBe("renamed-ops");
  });
});

describe("adoptRegistry preserves the reserved Captains tab", () => {
  it("re-injects the Captains tab when the server snapshot omits it", () => {
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.some((t) => t.id === CAPTAINS_TAB_ID)).toBe(true);
  });

  it("keeps agent tiles in the Captains tab and OUT of the server's work tabs", () => {
    // "cap" is an agent tile placed in the Captains tab; the server still lists
    // it live (in a work tab from its point of view). After the sync it must
    // stay in Captains and never reappear in the work tab.
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "cap"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === "t1")?.order).toEqual(["a"]);
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual(["cap"]);
    expect(s.terminals["cap"]).toBeDefined(); // preserved, not cleaned up
  });

  it("drops an agent tile the server no longer reports (server-closed captain)", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
    // The server snapshot no longer lists "cap" anywhere - it was killed.
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual([]);
    expect(s.terminals["cap"]).toBeUndefined();
  });
});

describe("reserved Captains tab is not closeable", () => {
  beforeEach(() => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
  });

  it("closeTab refuses the reserved tab", () => {
    const removed = useWorkspace.getState().closeTab(CAPTAINS_TAB_ID);
    expect(removed).toEqual([]);
    expect(
      useWorkspace.getState().tabs.some((t) => t.id === CAPTAINS_TAB_ID),
    ).toBe(true);
  });

  it("closeWorkspace refuses the reserved tab", () => {
    useWorkspace.getState().closeWorkspace(CAPTAINS_TAB_ID);
    expect(
      useWorkspace.getState().tabs.some((t) => t.id === CAPTAINS_TAB_ID),
    ).toBe(true);
  });
});

describe("Captains-tab placement helpers", () => {
  beforeEach(() => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a", "b"] },
        { id: "t2", name: "Workspace 2", order: ["c"] },
      ],
      "t1",
      "a",
    );
  });

  it("ensureCaptainsTab is idempotent (never a second reserved tab)", () => {
    const id1 = useWorkspace.getState().ensureCaptainsTab();
    const id2 = useWorkspace.getState().ensureCaptainsTab();
    expect(id1).toBe(CAPTAINS_TAB_ID);
    expect(id2).toBe(CAPTAINS_TAB_ID);
    expect(
      useWorkspace.getState().tabs.filter((t) => t.id === CAPTAINS_TAB_ID),
    ).toHaveLength(1);
  });

  it("moveTileToCaptainsTab pulls a tile from its work tab into the Captains tab", () => {
    useWorkspace.getState().moveTileToCaptainsTab("b");
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === "t1")?.order).toEqual(["a"]);
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toContain("b");
  });

  it("moveTileToWorkTab returns an agent tile to the first work tab", () => {
    useWorkspace.getState().moveTileToCaptainsTab("b");
    useWorkspace.getState().moveTileToWorkTab("b");
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).not.toContain("b");
    expect(s.tabs.find((t) => t.id === "t1")?.order).toContain("b");
  });

  it("moveTileToWorkTab is a no-op for a tile not in the Captains tab", () => {
    const before = useWorkspace.getState().tabs;
    useWorkspace.getState().moveTileToWorkTab("a"); // "a" is in a work tab
    expect(useWorkspace.getState().tabs).toBe(before);
  });
});

describe("adoptTerminal", () => {
  it("registers the live terminal without touching layout or focus", () => {
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().adoptTerminal(term("new1"));
    const s = useWorkspace.getState();
    expect(s.terminals["new1"]).toBeDefined();
    expect(s.focusedId).toBe("a");
    expect(s.tabs[0].order).toEqual(["a"]); // unplaced until the snapshot adopt
  });
});
