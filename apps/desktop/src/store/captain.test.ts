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
  CAPTAIN_DEFAULT_WIDTH,
  CAPTAIN_DEFAULT_HEIGHT,
} from "./captain";
import { useWorkspace, type WorkspaceTab } from "./workspace";
import type { TerminalInfo } from "../ipc/types";

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
  seedWorkspace();
  seedCaptains(["cap00001"]);
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
