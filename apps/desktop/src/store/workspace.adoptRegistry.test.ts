// Headless-org: the store's server-registry adopt path. The SERVER owns the
// tab/tile organization; `adoptRegistry` merges its snapshot into the store —
// these tests pin the no-focus-steal and lifecycle-cleanup semantics.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  registerCaptainRegistry,
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
    const snapshot = [
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
      { id: "t2", name: "hidden", tileIds: [] },
    ];
    useWorkspace.getState().adoptRegistry(snapshot);
    const before = useWorkspace.getState().tabs;
    useWorkspace.getState().adoptRegistry(snapshot);
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

describe("adoptRegistry keeps an externally-claimed captain (registry liveness)", () => {
  // The captain store registers this accessor at load; the tests drive it
  // directly so the workspace store's liveness fallback is exercised in isolation
  // (no captain-store coupling). Reset after each so it never leaks into the
  // suites above/below, which assume the empty default.
  afterEach(() => {
    registerCaptainRegistry(() => []);
  });

  it("keeps a captain tile in the registry even when the server omits it as a live work-tab tile", () => {
    // "cap" is an externally-claimed captain (e.g. the orchestrator claimed it
    // over the control socket): placed in the reserved Captains tab and present in
    // the captain registry, but the server's tab report never echoes it as a live
    // work-tab tile. It must survive the sync (this is the render bug being fixed).
    registerCaptainRegistry(() => ["cap"]);
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual(["cap"]);
    expect(s.terminals["cap"]).toBeDefined(); // preserved, not cleaned up
  });

  it("still drops a captain tile once it is gone from BOTH the server and the registry", () => {
    // Not in serverTileIds AND not in the registry (released via sync_captains):
    // genuinely gone, so it drops out of Captains and is cleaned up.
    registerCaptainRegistry(() => []);
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual([]);
    expect(s.terminals["cap"]).toBeUndefined();
  });
});

describe("adoptRegistry adopts a socket-commissioned captain from the reserved tab", () => {
  // THE DEFECT (agents-plane-captains): a captain commissioned over the control
  // socket - spawn_terminal with tabId=captains-reserved - has its tile placed by
  // the SERVER into the reserved Captains tab and its live terminal registered by
  // the spawn_terminal apply (adoptTerminal), but the client never pinned it, so
  // it is in NEITHER the local captains order NOR any work tab. The KEEP filter
  // only prunes the existing local order, so before the fix the tile was dropped
  // from every rebuilt tab: the agents plane rendered no tile and never attached a
  // PTY client (tmux session_attached=0). (The tile's live entry was NOT reaped by
  // the client cleanup pass, which only visits ids that were in a local tab; it
  // lingered unplaced - the gate that matters is the reserved-tab ORDER, asserted
  // below.)
  afterEach(() => {
    registerCaptainRegistry(() => []);
  });

  it("adopts an agent the server placed directly into captains-reserved that the local order lacks", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap1"] },
      ],
      "t1",
      "a",
    );
    // The live terminal is already registered (the spawn_terminal apply's
    // adoptTerminal), but "sock" is not yet in any tab order.
    useWorkspace.setState({
      terminals: { ...useWorkspace.getState().terminals, sock: term("sock") },
    });
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
      { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, tileIds: ["cap1", "sock"] },
    ]);
    const s = useWorkspace.getState();
    // The socket captain joins the agents plane at the tail, keeping the existing
    // one (so the plane renders + attaches its terminal like any other captain).
    // This ORDER assertion is what carries the gate: without the fix "sock" is
    // absent here.
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual([
      "cap1",
      "sock",
    ]);
    // Exactly one reserved tab, still last; not duplicated into a work tab.
    expect(s.tabs.filter((t) => t.id === CAPTAINS_TAB_ID)).toHaveLength(1);
    expect(s.tabs[s.tabs.length - 1].id).toBe(CAPTAINS_TAB_ID);
    expect(s.tabs.find((t) => t.id === "t1")?.order).toEqual(["a"]);
  });

  it("adopts the reserved-tab tile even before it is in the captains registry (spawn precedes claim)", () => {
    // The tile is placed at spawn time, BEFORE claim_captain registers it - so the
    // registry-liveness fallback does not yet cover it. The server's reserved-tab
    // placement alone must suffice.
    registerCaptainRegistry(() => []); // not a registered captain yet
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.setState({
      terminals: { ...useWorkspace.getState().terminals, sock: term("sock") },
    });
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
      { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, tileIds: ["sock"] },
    ]);
    const s = useWorkspace.getState();
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual(["sock"]);
  });

  it("does NOT re-adopt a captain the user just unpinned to a work tab (up-sync race)", () => {
    // The user unpinned captain "x": moveTileToWorkTab pulled its tile from the
    // reserved tab into a work tab LOCALLY, and that layout has not up-synced yet.
    // A server snapshot from the pre-unpin window still lists "x" in captains-
    // reserved (each tile lives in exactly one server tab, so the work tab is still
    // the server's ["a"]). The adopt loop must respect the local placement - keyed
    // on locallyPlaced, not merely the captains order - and NOT yank "x" back into
    // the agents plane. Keying on captainsOrder alone would re-adopt it and fight
    // the user's move on every sync until the report lands.
    registerCaptainRegistry(() => []); // released along with the unpin
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a", "x"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] },
      ],
      "t1",
      "a",
    );
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
      { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, tileIds: ["x"] },
    ]);
    const s = useWorkspace.getState();
    // The gate: "x" is NOT pulled back into the reserved tab (without the
    // locallyPlaced guard it would re-adopt as ["x"]). The server's work-tab view
    // (["a"]) then reconciles "x" out per its authority; the reporter's baseSeq
    // stale-rejection re-converges the user's pending move - both outside this
    // loop's concern.
    expect(s.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order).toEqual([]);
  });
});

describe("adoptRegistry never duplicates the reserved Captains tab (stray-placeholder bug)", () => {
  // ROOT CAUSE: the tab reporter up-syncs the client-only Captains tab to the
  // server, so the server echoes it back inside its snapshot. adoptRegistry used
  // to map EVERY server tab into `serverTabs` AND re-append a fresh Captains tab,
  // yielding TWO Captains tabs. The echoed copy's tiles are all agent tiles
  // (filtered out by agentSet), so its `order` is empty and it renders the stray
  // "new terminal" placeholder next to the real, populated Captains tab.
  it("collapses an echoed Captains tab into exactly one populated reserved tab", () => {
    seed(
      [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap"] },
      ],
      "t1",
      "a",
    );
    // The server snapshot ECHOES the reserved tab back (its agent tile listed
    // there), exactly as the running registry does after the reporter up-syncs it.
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
      { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, tileIds: ["cap"] },
    ]);
    const s = useWorkspace.getState();
    const reserved = s.tabs.filter((t) => t.id === CAPTAINS_TAB_ID);
    // Exactly one reserved tab...
    expect(reserved).toHaveLength(1);
    // ...and it keeps its agent tile (order non-empty -> no stray placeholder).
    expect(reserved[0].order).toEqual(["cap"]);
    // The reserved tab stays LAST.
    expect(s.tabs[s.tabs.length - 1].id).toBe(CAPTAINS_TAB_ID);
  });

  it("re-appends a single empty reserved tab when the server omits it (baseline)", () => {
    seed([{ id: "t1", name: "Workspace 1", order: ["a"] }], "t1", "a");
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a"] },
    ]);
    const reserved = useWorkspace
      .getState()
      .tabs.filter((t) => t.id === CAPTAINS_TAB_ID);
    expect(reserved).toHaveLength(1);
  });

  it("migrates the legacy label and rejects a conflicting Workspace kind", () => {
    seed([{ id: "t1", name: "Workspace 1", order: [] }], "t1", null);
    useWorkspace.getState().adoptRegistry([
      {
        schemaVersion: 1,
        id: CAPTAINS_TAB_ID,
        name: "Captains",
        kind: "captain",
        tileIds: [],
      },
      {
        schemaVersion: 1,
        id: "foreign",
        name: "Foreign",
        kind: "captain",
        tileIds: [],
      },
      { id: "t1", name: "Workspace 1", kind: "work", tileIds: [] },
    ]);
    const state = useWorkspace.getState();
    const captainWorkspace = state.tabs.find((tab) => tab.id === CAPTAINS_TAB_ID);
    expect(captainWorkspace).toMatchObject({
      schemaVersion: 1,
      kind: "captain",
      name: "Captain Workspace",
    });
    expect(state.tabs.some((tab) => tab.id === "foreign")).toBe(false);
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
