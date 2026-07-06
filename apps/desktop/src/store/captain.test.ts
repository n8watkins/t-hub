// Unit tests for the captain store's focus save/restore contract
// (captain-overlay fix round): the saved pre-summon focus id can go STALE
// while the overlay is open (the tile closed underneath it), so closeOverlay
// must validate it and fall back to the active tab's first tile.
import { describe, it, expect, beforeEach } from "vitest";
import { useCaptain, forgetCaptain } from "./captain";
import { useWorkspace } from "./workspace";

function seedWorkspace(): void {
  useWorkspace.setState({
    tabs: [
      { id: "t1", name: "Workspace 1", order: ["cap00001", "bbb00001"] },
      { id: "t2", name: "Workspace 2", order: ["ccc00001", "ddd00001"] },
    ],
    activeTabId: "t2",
    focusedId: "ccc00001",
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
