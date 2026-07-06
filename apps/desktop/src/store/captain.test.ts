// Unit tests for the captain store's focus save/restore contract
// (captain-overlay fix round): the saved pre-summon focus id can go STALE
// while the overlay is open (the tile closed underneath it), so closeOverlay
// must validate it and fall back to the active tab's first tile.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useCaptain, forgetCaptain } from "./captain";
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

beforeEach(() => {
  seedWorkspace();
  useCaptain.setState({
    captainId: "cap00001",
    open: false,
    x: null,
    y: null,
    width: 640,
    height: 400,
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
});

describe("designation lifecycle", () => {
  it("unpinning while summoned closes the overlay and restores focus", () => {
    useCaptain.getState().openOverlay();
    useCaptain.getState().toggleCaptain("cap00001"); // unpin
    expect(useCaptain.getState().captainId).toBeNull();
    expect(useCaptain.getState().open).toBe(false);
    expect(useWorkspace.getState().focusedId).toBe("ccc00001");
  });

  it("forgetCaptain unpins only the matching terminal", () => {
    forgetCaptain("bbb00001");
    expect(useCaptain.getState().captainId).toBe("cap00001");
    forgetCaptain("cap00001");
    expect(useCaptain.getState().captainId).toBeNull();
  });
});

describe("server registry adoption (headless-org PR #8)", () => {
  // adoptRegistry replaces tab records wholesale from the server snapshot.
  // The designation lives in the captain store keyed by session id, so it
  // must survive any adoption that keeps the captain's tile SOMEWHERE - and
  // when an adoption drops the tile from every tab, the adopt path's
  // cleanupTileSideState -> forgetCaptain (async, dynamic import) must unpin
  // and dismiss so the overlay never points at a headlessly-closed session.
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
    expect(useCaptain.getState().captainId).toBe("cap00001");
    expect(useCaptain.getState().open).toBe(true);
  });

  it("unpins and dismisses when adoption drops the captain's tile from every tab", async () => {
    useCaptain.getState().openOverlay();
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["bbb00001"] },
      { id: "t2", name: "Workspace 2", tileIds: ["ccc00001", "ddd00001"] },
    ]);
    await vi.waitFor(() => {
      expect(useCaptain.getState().captainId).toBeNull();
      expect(useCaptain.getState().open).toBe(false);
    });
  });
});
