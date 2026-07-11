// Unit tests for the captain store (captain-overlay + captain-list phase 1):
//   - the focus save/restore contract (the saved pre-summon id can go STALE
//     while the overlay is open, so closeOverlay must validate + fall back);
//   - the v1 -> v2 persistence migration (a single pin becomes a one-entry
//     list; an existing pin is never lost; an explicit v2 unpin never
//     resurrects the stale v1 pin);
//   - the MRU list semantics: summon = move-to-front, cycle = ROTATE
//     (round-robin through every pinned captain, no ping-pong);
//   - unpin / kill / adoption-drop lifecycle (unpinning the SUMMONED captain
//     closes the overlay; other pins survive).
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  useCaptain,
  forgetCaptain,
  loadCaptainPersisted,
  agentOrder,
  CAPTAIN_DEFAULT_WIDTH,
  CAPTAIN_DEFAULT_HEIGHT,
  type CaptainClaimRecord,
} from "./captain";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  type WorkspaceTab,
} from "./workspace";
import type { TerminalInfo } from "../ipc/types";

// Capture the store's fire-and-forget server captaincy mutations (phase 2:
// pin = claim_captain, unpin = release_captain) without a control channel.
const controlRequests: Array<{ command: string; args: unknown }> = [];
vi.mock("../ipc/controlClient", () => ({
  controlRequest: (command: string, args: unknown) => {
    controlRequests.push({ command, args });
    return Promise.resolve({});
  },
  onControlEvent: () => () => {},
}));

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

function seedWorkspace(): void {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["cap00001", "bbb00001"] },
    { id: "t2", name: "Workspace 2", order: ["ccc00001", "ddd00001"] },
  ];
  const terminals: Record<string, TerminalInfo> = {};
  for (const t of tabs) for (const id of t.order) terminals[id] = term(id);
  useWorkspace.setState({
    tabs,
    activeTabId: "t2",
    focusedId: "ccc00001",
    terminals,
    poppedOutTabs: [],
  });
}

function seedCaptains(ids: string[]): void {
  useCaptain.setState({
    captainIds: ids,
    activeCaptainId: ids[0] ?? null,
    open: false,
    anchorMenuOpen: false,
    x: null,
    y: null,
    width: 640,
    height: 400,
  });
}

beforeEach(() => {
  localStorage.clear();
  controlRequests.length = 0;
  seedWorkspace();
  seedCaptains(["cap00001"]);
  useCaptain.setState({
    claims: {},
    orchestratorId: null,
  });
});

describe("v1 -> v2 persistence migration", () => {
  it("migrates a v1 single pin into a one-entry v2 list, keeping geometry", () => {
    localStorage.setItem(
      "t-hub.captain.v1",
      JSON.stringify({ captainId: "cap00001", x: 12, y: 34, width: 700, height: 450 }),
    );
    const p = loadCaptainPersisted();
    expect(p.captainIds).toEqual(["cap00001"]);
    expect(p.x).toBe(12);
    expect(p.y).toBe(34);
    expect(p.width).toBe(700);
    expect(p.height).toBe(450);
  });

  it("migrates an empty v1 blob (no pin) to an empty list with defaults", () => {
    localStorage.setItem(
      "t-hub.captain.v1",
      JSON.stringify({ captainId: null, x: null, y: null, width: 640, height: 400 }),
    );
    const p = loadCaptainPersisted();
    expect(p.captainIds).toEqual([]);
  });

  it("prefers a present v2 blob over the legacy v1 pin", () => {
    localStorage.setItem(
      "t-hub.captain.v1",
      JSON.stringify({ captainId: "old00001", x: 1, y: 1, width: 640, height: 400 }),
    );
    localStorage.setItem(
      "t-hub.captain.v2",
      JSON.stringify({ captainIds: ["new00001", "new00002"], x: 5, y: 6, width: 800, height: 500 }),
    );
    const p = loadCaptainPersisted();
    expect(p.captainIds).toEqual(["new00001", "new00002"]);
    expect(p.x).toBe(5);
  });

  it("does NOT resurrect the v1 pin when v2 holds an explicitly empty list", () => {
    // The user migrated, then unpinned: the empty v2 list is the truth.
    localStorage.setItem(
      "t-hub.captain.v1",
      JSON.stringify({ captainId: "old00001", x: 1, y: 1, width: 640, height: 400 }),
    );
    localStorage.setItem(
      "t-hub.captain.v2",
      JSON.stringify({ captainIds: [], x: 1, y: 1, width: 640, height: 400 }),
    );
    expect(loadCaptainPersisted().captainIds).toEqual([]);
  });

  it("falls back to the migrated v1 pin when the v2 blob is corrupt", () => {
    localStorage.setItem(
      "t-hub.captain.v1",
      JSON.stringify({ captainId: "old00001", x: 1, y: 1, width: 640, height: 400 }),
    );
    localStorage.setItem("t-hub.captain.v2", "{not json");
    expect(loadCaptainPersisted().captainIds).toEqual(["old00001"]);
  });

  it("returns empty defaults when neither key exists", () => {
    const p = loadCaptainPersisted();
    expect(p.captainIds).toEqual([]);
    expect(p.width).toBe(CAPTAIN_DEFAULT_WIDTH);
    expect(p.height).toBe(CAPTAIN_DEFAULT_HEIGHT);
  });

  it("sanitizes a malformed v2 list (dedupes, drops non-strings)", () => {
    localStorage.setItem(
      "t-hub.captain.v2",
      JSON.stringify({ captainIds: ["a", "a", 7, "", "b"], width: 640, height: 400 }),
    );
    expect(loadCaptainPersisted().captainIds).toEqual(["a", "b"]);
  });
});

describe("openOverlay / closeOverlay focus contract", () => {
  it("moves focus to the captain on open and restores it on close", () => {
    useCaptain.getState().openOverlay();
    expect(useCaptain.getState().open).toBe(true);
    expect(useWorkspace.getState().focusedId).toBe("cap00001");

    useCaptain.getState().closeOverlay();
    expect(useCaptain.getState().open).toBe(false);
    expect(useWorkspace.getState().focusedId).toBe("ccc00001");
  });

  it("falls back to the active tab's first tile when the saved id is stale", () => {
    useCaptain.getState().openOverlay();
    // The pre-summon tile (ccc00001) is closed while the overlay is open.
    useWorkspace.setState({
      tabs: [
        { id: "t1", name: "Workspace 1", order: ["cap00001", "bbb00001"] },
        { id: "t2", name: "Workspace 2", order: ["ddd00001"] },
      ],
    });

    useCaptain.getState().closeOverlay();
    expect(useWorkspace.getState().focusedId).toBe("ddd00001");
  });

  it("leaves focus alone when the active tab has no tiles at all", () => {
    useCaptain.getState().openOverlay();
    useWorkspace.setState({
      tabs: [
        { id: "t1", name: "Workspace 1", order: ["cap00001"] },
        { id: "t2", name: "Workspace 2", order: [] },
      ],
    });

    useCaptain.getState().closeOverlay();
    // Stale prev + empty active tab: no candidate, focus untouched (captain).
    expect(useWorkspace.getState().focusedId).toBe("cap00001");
  });

  it("does not open without a live captain tile", () => {
    useWorkspace.setState({
      tabs: [{ id: "t2", name: "Workspace 2", order: ["ccc00001"] }],
      activeTabId: "t2",
    });
    useCaptain.getState().openOverlay();
    expect(useCaptain.getState().open).toBe(false);
  });

  it("skips a tile-less pin and summons the next live captain (MRU order)", () => {
    seedCaptains(["gone0001", "cap00001"]);
    useCaptain.getState().openOverlay();
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
    // The summoned captain moved to the MRU front; the dead pin stays pinned.
    expect(useCaptain.getState().captainIds).toEqual(["cap00001", "gone0001"]);
  });
});

describe("pinning is additive", () => {
  it("pin appends without touching the active captain; unpin removes only that pin", () => {
    useCaptain.getState().pinCaptain("bbb00001");
    expect(useCaptain.getState().captainIds).toEqual(["cap00001", "bbb00001"]);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");

    useCaptain.getState().unpinCaptain("bbb00001");
    expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("toggleCaptain pins an unpinned tile and unpins a pinned one", () => {
    useCaptain.getState().toggleCaptain("bbb00001"); // pin
    expect(useCaptain.getState().captainIds).toEqual(["cap00001", "bbb00001"]);
    useCaptain.getState().toggleCaptain("bbb00001"); // unpin
    expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
  });
});

describe("summon + cycle order (MRU)", () => {
  beforeEach(() => {
    seedCaptains(["cap00001", "bbb00001", "ddd00001"]);
  });

  it("toggleOverlay while summoned CYCLES (round-robin) instead of dismissing", () => {
    useCaptain.getState().toggleOverlay(); // summon active (cap00001)
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");

    useCaptain.getState().toggleOverlay(); // cycle -> bbb00001
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("bbb00001");
    expect(useWorkspace.getState().focusedId).toBe("bbb00001");
    expect(useCaptain.getState().captainIds).toEqual([
      "bbb00001",
      "ddd00001",
      "cap00001",
    ]);

    useCaptain.getState().toggleOverlay(); // cycle -> ddd00001
    expect(useCaptain.getState().activeCaptainId).toBe("ddd00001");

    useCaptain.getState().toggleOverlay(); // cycle -> wraps to cap00001
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("an explicit summon moves that captain to the MRU front; the next cycle visits the previously most recent", () => {
    useCaptain.getState().summonCaptain("ddd00001");
    expect(useCaptain.getState().captainIds).toEqual([
      "ddd00001",
      "cap00001",
      "bbb00001",
    ]);
    useCaptain.getState().cycleCaptain();
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("cycle skips a pinned captain whose tile is gone", () => {
    seedCaptains(["cap00001", "gone0001", "ddd00001"]);
    useCaptain.getState().openOverlay();
    useCaptain.getState().cycleCaptain();
    expect(useCaptain.getState().activeCaptainId).toBe("ddd00001");
  });

  it("cycling with a single captain is a no-op (stays summoned; Esc dismisses)", () => {
    seedCaptains(["cap00001"]);
    useCaptain.getState().openOverlay();
    useCaptain.getState().toggleOverlay();
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("cycling does not disturb the pre-summon focus restore", () => {
    useCaptain.getState().openOverlay(); // saved prev focus: ccc00001
    useCaptain.getState().cycleCaptain(); // -> bbb00001
    useCaptain.getState().closeOverlay();
    expect(useWorkspace.getState().focusedId).toBe("ccc00001");
  });
});

describe("designation lifecycle", () => {
  it("unpinning the summoned captain closes the overlay and restores focus", () => {
    useCaptain.getState().openOverlay();
    useCaptain.getState().toggleCaptain("cap00001"); // unpin
    expect(useCaptain.getState().captainIds).toEqual([]);
    expect(useCaptain.getState().activeCaptainId).toBeNull();
    expect(useCaptain.getState().open).toBe(false);
    expect(useWorkspace.getState().focusedId).toBe("ccc00001");
  });

  it("unpinning the summoned captain keeps the other pins; the next MRU pin becomes active", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    useCaptain.getState().openOverlay();
    useCaptain.getState().unpinCaptain("cap00001");
    expect(useCaptain.getState().open).toBe(false);
    expect(useCaptain.getState().captainIds).toEqual(["bbb00001"]);
    expect(useCaptain.getState().activeCaptainId).toBe("bbb00001");
    expect(useWorkspace.getState().focusedId).toBe("ccc00001");
  });

  it("unpinning a NON-summoned captain leaves the overlay up", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    useCaptain.getState().openOverlay();
    useCaptain.getState().unpinCaptain("bbb00001");
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
    expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
  });

  it("unpinning persists the updated list to t-hub.captain.v2 (round-trip)", () => {
    // Guards commitIds' persist call: a dropped write would leave a ghost pin
    // to resurrect on the next app start.
    useCaptain.getState().unpinCaptain("cap00001");
    const raw = localStorage.getItem("t-hub.captain.v2");
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw!).captainIds).toEqual([]);
    expect(loadCaptainPersisted().captainIds).toEqual([]);
  });

  it("unpinning the LAST captain closes the anchor dropdown (no orphaned popover)", () => {
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().unpinCaptain("cap00001");
    expect(useCaptain.getState().captainIds).toEqual([]);
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
  });

  it("unpinning with pins remaining leaves the anchor dropdown up", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().unpinCaptain("bbb00001");
    expect(useCaptain.getState().anchorMenuOpen).toBe(true);
  });

  it("summoning (chord or palette) retires the anchor dropdown", () => {
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().summonCaptain("cap00001");
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(useCaptain.getState().open).toBe(true);

    useCaptain.getState().closeOverlay();
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().openOverlay();
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
  });

  it("re-summoning the MRU-front captain skips the redundant persist write", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    useCaptain.getState().summonCaptain("cap00001"); // writes (open flip aside)
    localStorage.removeItem("t-hub.captain.v2");
    useCaptain.getState().summonCaptain("cap00001"); // already front: no write
    expect(localStorage.getItem("t-hub.captain.v2")).toBeNull();
    useCaptain.getState().summonCaptain("bbb00001"); // reorder: writes
    expect(localStorage.getItem("t-hub.captain.v2")).not.toBeNull();
  });

  it("forgetCaptain (kill path) unpins only the matching terminal", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    forgetCaptain("ccc00001"); // not pinned: no-op
    expect(useCaptain.getState().captainIds).toEqual(["cap00001", "bbb00001"]);
    forgetCaptain("bbb00001");
    expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
    forgetCaptain("cap00001");
    expect(useCaptain.getState().captainIds).toEqual([]);
    expect(useCaptain.getState().activeCaptainId).toBeNull();
  });
});

describe("server registry adoption (headless-org PR #8)", () => {
  // adoptRegistry replaces tab records wholesale from the server snapshot.
  // The designations live in the captain store keyed by session id, so they
  // must survive any adoption that keeps the captain's tile SOMEWHERE - and
  // when an adoption drops a pinned captain's tile from every tab, the adopt
  // path's cleanupTileSideState -> forgetCaptain (async, dynamic import) must
  // unpin it (and dismiss the overlay if it was the summoned one) so the
  // overlay never points at a headlessly-closed session.
  it("keeps the designation and the summoned overlay across an adoption", async () => {
    useCaptain.getState().openOverlay();
    useWorkspace.getState().adoptRegistry([
      // Captain's tile moves to another tab; an unrelated tile is dropped.
      { id: "t2", name: "Workspace 2", tileIds: ["ccc00001", "cap00001"] },
      { id: "t3", name: "staging", tileIds: ["ddd00001"] },
    ]);
    // Give the dropped tile's async cleanup a chance to run - it must NOT
    // touch the captain.
    await new Promise((r) => setTimeout(r, 0));
    expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
    expect(useCaptain.getState().open).toBe(true);
  });

  it("unpins and dismisses when adoption drops the summoned captain's tile from every tab", async () => {
    useCaptain.getState().openOverlay();
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["bbb00001"] },
      { id: "t2", name: "Workspace 2", tileIds: ["ccc00001", "ddd00001"] },
    ]);
    await vi.waitFor(() => {
      expect(useCaptain.getState().captainIds).toEqual([]);
      expect(useCaptain.getState().activeCaptainId).toBeNull();
      expect(useCaptain.getState().open).toBe(false);
    });
  });

  it("adoption-drop of a NON-summoned pin unpins only it; the overlay stays up", async () => {
    seedCaptains(["cap00001", "ddd00001"]);
    useCaptain.getState().openOverlay(); // shows cap00001
    useWorkspace.getState().adoptRegistry([
      // ddd00001 (the other pinned captain) is dropped everywhere.
      { id: "t1", name: "Workspace 1", tileIds: ["cap00001", "bbb00001"] },
      { id: "t2", name: "Workspace 2", tileIds: ["ccc00001"] },
    ]);
    await vi.waitFor(() => {
      expect(useCaptain.getState().captainIds).toEqual(["cap00001"]);
    });
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
    expect(useCaptain.getState().open).toBe(true);
  });
});

describe("phase 2: pinning is claiming (server captains registry)", () => {
  const claim = (
    id: string,
    tabs: string[] = [],
    crew: string[] = [],
  ): CaptainClaimRecord => ({
    terminalId: id,
    shipSlug: `ship-${id}`,
    workspaceTabIds: tabs,
    crew: crew.map((c) => ({ terminalId: c })),
  });

  it("pin fires claim_captain and unpin fires release_captain (best-effort)", async () => {
    useCaptain.getState().pinCaptain("bbb00001");
    await vi.waitFor(() => {
      expect(controlRequests).toEqual([
        { command: "claim_captain", args: { captainSessionId: "bbb00001" } },
      ]);
    });
    useCaptain.getState().unpinCaptain("bbb00001");
    await vi.waitFor(() => {
      expect(controlRequests[1]).toEqual({
        command: "release_captain",
        args: { captainSessionId: "bbb00001" },
      });
    });
    // Re-pinning an already-pinned id is a full no-op (no duplicate claim).
    useCaptain.getState().pinCaptain("cap00001");
    await new Promise((r) => setTimeout(r, 0));
    expect(controlRequests).toHaveLength(2);
  });

  it("adoption preserves local MRU for survivors and appends new claims at the tail", () => {
    seedCaptains(["ddd00001", "cap00001"]);
    useCaptain.getState().adoptCaptainsRegistry([
      claim("cap00001", ["t1"]),
      claim("bbb00001", ["t2"], ["crew0001"]),
      claim("ddd00001"),
    ]);
    const s = useCaptain.getState();
    // Survivors keep the LOCAL MRU order (ddd before cap despite server order).
    expect(s.captainIds).toEqual(["ddd00001", "cap00001", "bbb00001"]);
    expect(s.activeCaptainId).toBe("ddd00001");
    expect(s.claims["bbb00001"].crew).toEqual([{ terminalId: "crew0001" }]);
    expect(s.claims["cap00001"].workspaceTabIds).toEqual(["t1"]);
  });

  it("adoption drops a local pin the server no longer holds, closing the overlay if summoned", () => {
    seedCaptains(["cap00001", "bbb00001"]);
    useCaptain.getState().openOverlay(); // summons cap00001
    useCaptain.getState().adoptCaptainsRegistry([claim("bbb00001")]);
    const s = useCaptain.getState();
    expect(s.captainIds).toEqual(["bbb00001"]);
    expect(s.open).toBe(false);
    expect(useWorkspace.getState().focusedId).toBe("ccc00001"); // focus restored
  });

  it("adoption of an unchanged membership refreshes claims without a persist write", () => {
    localStorage.removeItem("t-hub.captain.v2");
    useCaptain.getState().adoptCaptainsRegistry([claim("cap00001", ["t1"])]);
    expect(useCaptain.getState().claims["cap00001"].workspaceTabIds).toEqual(["t1"]);
    expect(localStorage.getItem("t-hub.captain.v2")).toBeNull();
  });

  it("Ctrl+B C summons the captain OWNING the active workspace tab first", () => {
    // MRU order says cap00001, but the active tab (t2) is claimed by ddd00001.
    seedCaptains(["cap00001", "ddd00001"]);
    useCaptain.getState().adoptCaptainsRegistry([
      claim("cap00001", ["t1"]),
      claim("ddd00001", ["t2"]),
    ]);
    useCaptain.getState().toggleOverlay();
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("ddd00001");
    expect(useCaptain.getState().captainIds).toEqual(["ddd00001", "cap00001"]);
  });

  it("summoning from an UNCLAIMED tab falls back to MRU", () => {
    seedCaptains(["cap00001", "ddd00001"]);
    useCaptain.getState().adoptCaptainsRegistry([
      claim("cap00001", ["t1"]),
      claim("ddd00001", []), // nobody claims the active tab t2
    ]);
    useCaptain.getState().toggleOverlay();
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("MRU breaks a tie between multiple owners of the active tab", () => {
    seedCaptains(["ddd00001", "cap00001"]);
    useCaptain.getState().adoptCaptainsRegistry([
      claim("cap00001", ["t2"]),
      claim("ddd00001", ["t2"]),
    ]);
    useCaptain.getState().toggleOverlay();
    expect(useCaptain.getState().activeCaptainId).toBe("ddd00001");
  });

  it("an owner whose tile is gone yields to the MRU fallback", () => {
    seedCaptains(["gone0001", "cap00001"]);
    useCaptain.getState().adoptCaptainsRegistry([
      claim("gone0001", ["t2"]), // owns the active tab but has no live tile
      claim("cap00001", ["t1"]),
    ]);
    useCaptain.getState().toggleOverlay();
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });

  it("IGNORES a spurious empty snapshot while local pins exist and no release is in flight (A1 guard)", () => {
    // A newer zero-captain snapshot from a registry load failure / reconnect-
    // before-load must NOT wipe the persisted designations (the migration seed).
    localStorage.removeItem("t-hub.captain.v2");
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().adoptCaptainsRegistry([]);
    const s = useCaptain.getState();
    expect(s.captainIds).toEqual(["cap00001"]); // pins KEPT
    expect(s.activeCaptainId).toBe("cap00001");
    // Nothing was cleared, so nothing was persisted (the empty list never wrote).
    expect(localStorage.getItem("t-hub.captain.v2")).toBeNull();
  });

  it("adopts an empty snapshot when the local store is already empty (legitimate clear)", () => {
    // The legitimate empty path: unpinning the last captain clears the store
    // FIRST, so by the time the empty snapshot arrives the store is already
    // empty - the guard does not fire and the adopt is a clean no-op.
    seedCaptains([]);
    useCaptain.setState({ claims: {} });
    useCaptain.getState().setAnchorMenu(true);
    useCaptain.getState().adoptCaptainsRegistry([]);
    const s = useCaptain.getState();
    expect(s.captainIds).toEqual([]);
    expect(s.anchorMenuOpen).toBe(false);
  });
});

describe("orchestrator designation", () => {
  it("setOrchestratorId designates the terminal and persists (round-trip)", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    expect(useCaptain.getState().orchestratorId).toBe("cap00001");
    // Persisted to the v2 blob and re-read on load (survives a relaunch).
    const raw = localStorage.getItem("t-hub.captain.v2");
    expect(JSON.parse(raw!).orchestratorId).toBe("cap00001");
    expect(loadCaptainPersisted().orchestratorId).toBe("cap00001");
  });

  it("designating the orchestrator moves its tile INTO the reserved Captains tab", () => {
    // cap00001 starts in the work tab "t1".
    useCaptain.getState().setOrchestratorId("cap00001");
    const ws = useWorkspace.getState();
    const captains = ws.tabs.find((t) => t.id === CAPTAINS_TAB_ID);
    expect(captains?.order).toContain("cap00001");
    // ...and OUT of the work tab.
    expect(ws.tabs.find((t) => t.id === "t1")?.order).not.toContain("cap00001");
  });

  it("un-designating the orchestrator returns its tile to a work tab", () => {
    // ccc00001 is a plain tile (not a pinned captain), so clearing the
    // orchestrator returns it to a work tab.
    useCaptain.getState().setOrchestratorId("ccc00001");
    useCaptain.getState().setOrchestratorId(null);
    const ws = useWorkspace.getState();
    expect(
      ws.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order,
    ).not.toContain("ccc00001");
    // Back in a normal (non-reserved) work tab.
    const workTab = ws.tabs.find(
      (t) => t.id !== CAPTAINS_TAB_ID && t.order.includes("ccc00001"),
    );
    expect(workTab).toBeTruthy();
  });

  it("pinning a captain moves its tile into the Captains tab; unpinning returns it", () => {
    useCaptain.getState().pinCaptain("ddd00001");
    expect(
      useWorkspace.getState().tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order,
    ).toContain("ddd00001");
    useCaptain.getState().unpinCaptain("ddd00001");
    const ws = useWorkspace.getState();
    expect(
      ws.tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order,
    ).not.toContain("ddd00001");
    expect(
      ws.tabs.some(
        (t) => t.id !== CAPTAINS_TAB_ID && t.order.includes("ddd00001"),
      ),
    ).toBe(true);
  });

  it("unpinning a captain that is STILL the orchestrator keeps its tile in the Captains tab", () => {
    useCaptain.getState().setOrchestratorId("ddd00001");
    useCaptain.getState().pinCaptain("ddd00001");
    useCaptain.getState().unpinCaptain("ddd00001"); // still orchestrator
    expect(
      useWorkspace.getState().tabs.find((t) => t.id === CAPTAINS_TAB_ID)?.order,
    ).toContain("ddd00001");
  });

  it("setOrchestratorId(null) clears the designation and persists the clear", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    useCaptain.getState().setOrchestratorId(null);
    expect(useCaptain.getState().orchestratorId).toBeNull();
    expect(loadCaptainPersisted().orchestratorId).toBeNull();
  });

  it("re-designating the same id is a no-op (no redundant persist write)", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    localStorage.removeItem("t-hub.captain.v2");
    useCaptain.getState().setOrchestratorId("cap00001"); // unchanged
    expect(localStorage.getItem("t-hub.captain.v2")).toBeNull();
  });

  it("forgetCaptain clears the orchestrator when ITS terminal dies", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    forgetCaptain("cap00001");
    expect(useCaptain.getState().orchestratorId).toBeNull();
  });

  it("forgetCaptain leaves a different terminal's orchestrator designation intact", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    forgetCaptain("bbb00001"); // a different tile closing
    expect(useCaptain.getState().orchestratorId).toBe("cap00001");
  });

  it("the orchestrator can be any tile, not only a pinned captain", () => {
    // ccc00001 is a live tile but not pinned as a captain.
    expect(useCaptain.getState().captainIds).not.toContain("ccc00001");
    useCaptain.getState().setOrchestratorId("ccc00001");
    expect(useCaptain.getState().orchestratorId).toBe("ccc00001");
    expect(loadCaptainPersisted().orchestratorId).toBe("ccc00001");
  });
});

describe("orchestrator reconcile on adopt", () => {
  it("clears a STALE orchestrator whose terminal is no longer present", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    // A relaunch where cap00001's session did NOT return: the workspace has
    // terminals, but not that one.
    useWorkspace.setState({ terminals: { bbb00001: term("bbb00001") } });
    useCaptain.getState().adoptCaptainsRegistry([
      { terminalId: "bbb00001", shipSlug: "s", workspaceTabIds: [], crew: [] },
    ]);
    expect(useCaptain.getState().orchestratorId).toBeNull();
  });

  it("keeps a LIVE orchestrator that is still present", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    useWorkspace.setState({ terminals: { cap00001: term("cap00001") } });
    useCaptain.getState().adoptCaptainsRegistry([
      { terminalId: "cap00001", shipSlug: "s", workspaceTabIds: [], crew: [] },
    ]);
    expect(useCaptain.getState().orchestratorId).toBe("cap00001");
  });

  it("does NOT clear the orchestrator when the workspace has no terminals yet (boot)", () => {
    useCaptain.getState().setOrchestratorId("cap00001");
    useWorkspace.setState({ terminals: {} }); // not loaded yet
    useCaptain.getState().adoptCaptainsRegistry([
      { terminalId: "cap00001", shipSlug: "s", workspaceTabIds: [], crew: [] },
    ]);
    expect(useCaptain.getState().orchestratorId).toBe("cap00001");
  });
});

describe("agent hierarchy", () => {
  it("agentOrder puts the orchestrator FIRST, then captains, deduped", () => {
    expect(agentOrder({ orchestratorId: null, captainIds: ["a", "b"] })).toEqual([
      "a",
      "b",
    ]);
    expect(agentOrder({ orchestratorId: "o", captainIds: ["a", "b"] })).toEqual([
      "o",
      "a",
      "b",
    ]);
    // The orchestrator may itself be a captain - not listed twice.
    expect(agentOrder({ orchestratorId: "b", captainIds: ["a", "b"] })).toEqual([
      "b",
      "a",
    ]);
  });
});
