// Headless-org: the store's server-registry adopt path. The SERVER owns the
// tab/tile organization; `adoptRegistry` merges its snapshot into the store —
// these tests pin the no-focus-steal and lifecycle-cleanup semantics.
import { beforeEach, describe, expect, it } from "vitest";
import { useWorkspace, type WorkspaceTab } from "./workspace";
import type { TerminalInfo } from "../ipc/types";

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
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", "t2", "t3"]);
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
    expect(s.tabs.map((t) => t.id)).toEqual(["t1"]);
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
    expect(useWorkspace.getState().tabs).toHaveLength(2);
  });

  it("applies a rename from the registry", () => {
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "renamed-ops", tileIds: [] },
    ]);
    expect(useWorkspace.getState().tabs[1].name).toBe("renamed-ops");
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
