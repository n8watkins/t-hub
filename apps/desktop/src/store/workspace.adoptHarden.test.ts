// adopt-harden regression: a garbage/debris session must NEVER get adopted onto
// the canvas once the SERVER registry is authoritative. This pins the exact gate
// behind the incident - `setTerminals` blind-appended EVERY unplaced live `th_*`
// session (including 13 leaked `th_s27churn*` ghosts) onto the active tab, which
// then blanked the UI. With the server authoritative (`registryAdopted`), only the
// tiles the server places are on the canvas; debris is left unplaced (so it never
// renders a tile nor attaches a PTY), while the legacy blind-append survives ONLY
// for a registry-less boot.
import { beforeEach, describe, expect, it } from "vitest";
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

function seed(
  tabs: WorkspaceTab[],
  activeTabId: string,
  registryAdopted: boolean,
): void {
  const withReserved = tabs.some((t) => t.id === CAPTAINS_TAB_ID)
    ? tabs
    : [...tabs, { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] }];
  useWorkspace.setState({
    tabs: withReserved,
    activeTabId,
    focusedId: null,
    terminals: {},
    poppedOutTabs: [],
    registryAdopted,
  });
}

function placedIds(): Set<string> {
  return new Set(useWorkspace.getState().tabs.flatMap((t) => t.order));
}

describe("adopt-harden: debris never reaches the canvas once the server is authoritative", () => {
  beforeEach(() => {
    registerCaptainRegistry(() => []);
    seed([{ id: "t1", name: "Workspace 1", order: ["a", "b"] }], "t1", false);
  });

  it("marks the registry authoritative once a non-empty server snapshot is adopted", () => {
    expect(useWorkspace.getState().registryAdopted).toBe(false);
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
    ]);
    expect(useWorkspace.getState().registryAdopted).toBe(true);
  });

  it("does NOT re-adopt an empty snapshot as authoritative", () => {
    useWorkspace.getState().adoptRegistry([]);
    expect(useWorkspace.getState().registryAdopted).toBe(false);
  });

  it("N healthy + M ghost sessions -> only the N healthy tiles stay placed (attach)", () => {
    // The server registry is authoritative: two healthy work tiles.
    useWorkspace.getState().adoptRegistry([
      { id: "t1", name: "Workspace 1", tileIds: ["a", "b"] },
    ]);
    expect(useWorkspace.getState().registryAdopted).toBe(true);

    // The Canvas-mount reconcile then hands setTerminals the FULL live tmux set:
    // the 2 healthy sessions + 3 leaked ghost sessions (shaped like the incident's
    // `th_s27churn<ns>` debris) the server never placed.
    useWorkspace.getState().setTerminals([
      term("a"),
      term("b"),
      term("s27churn1730000000000000001"),
      term("s27churn1730000000000000002"),
      term("s27churn1730000000000000003"),
    ]);

    const placed = placedIds();
    // The N healthy tiles are still placed -> they render + attach.
    expect(placed.has("a")).toBe(true);
    expect(placed.has("b")).toBe(true);
    // None of the M ghosts were dumped onto the active tab (the incident gate).
    expect(placed.has("s27churn1730000000000000001")).toBe(false);
    expect(placed.has("s27churn1730000000000000002")).toBe(false);
    expect(placed.has("s27churn1730000000000000003")).toBe(false);
    // The active tab is untouched by the debris.
    expect(useWorkspace.getState().tabs.find((t) => t.id === "t1")?.order).toEqual([
      "a",
      "b",
    ]);
  });

  it("still adopts pre-existing sessions on a registry-less boot (legacy fallback preserved)", () => {
    // No server registry has arrived yet: the blind-append must still surface a
    // pre-existing session so a registry-less first boot is not blank.
    expect(useWorkspace.getState().registryAdopted).toBe(false);
    useWorkspace.getState().setTerminals([term("a"), term("b"), term("preexisting")]);
    expect(placedIds().has("preexisting")).toBe(true);
    expect(useWorkspace.getState().tabs.find((t) => t.id === "t1")?.order).toContain(
      "preexisting",
    );
  });

  it("recovers ordinary shells out of Captain Workspace during a cold boot", () => {
    seed(
      [{ id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["ordinary-shell"] }],
      CAPTAINS_TAB_ID,
      false,
    );

    useWorkspace.getState().setTerminals([term("ordinary-shell")]);

    const state = useWorkspace.getState();
    expect(state.tabs.find((tab) => tab.id === CAPTAINS_TAB_ID)?.order).toEqual([]);
    expect(state.tabs.some((tab) => tab.id !== CAPTAINS_TAB_ID)).toBe(true);
    expect(state.tabs.find((tab) => tab.id !== CAPTAINS_TAB_ID)?.order).toEqual([
      "ordinary-shell",
    ]);
    expect(state.activeTabId).not.toBe(CAPTAINS_TAB_ID);
  });
});
