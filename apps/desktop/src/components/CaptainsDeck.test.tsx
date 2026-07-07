// The CAPTAINS DECK (orchestrator UI): the deck tiles every pinned captain with
// its stable identity, status dot, and crew summary; clicking a tile summons the
// captain and closes the deck. These tests pin the tiling, the stable-identity
// derivation at the component level (the Claude title never leaks), and the
// summon-on-click wiring.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, fireEvent, render } from "@testing-library/react";

// CaptainsDeck -> CaptainOverlay (status dot) -> TerminalPool -> xterm, whose
// import-time color math needs a real <canvas>. Nothing here renders a real
// terminal, so stub the pool to keep xterm out of jsdom.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { CaptainsDeck } from "./CaptainsDeck";
import { useCaptain, type CaptainClaimRecord } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { useSupervision } from "../store/supervision";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

function claim(id: string, crew: string[] = []): CaptainClaimRecord {
  return { captainSessionId: id, shipSlug: `ship-${id}`, workspaceTabIds: [], crew };
}

function tile(terminalId: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(
    `[data-deck-tile="${terminalId}"]`,
  );
  expect(el).toBeTruthy();
  return el!;
}

beforeEach(() => {
  localStorage.clear();
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["cap00001", "crewrun0"] },
    { id: "t2", name: "Backend", order: ["cap00002"] },
  ];
  const terminals: Record<string, TerminalInfo> = {
    cap00001: term("cap00001"),
    cap00002: term("cap00002"),
    crewrun0: term("crewrun0"),
  };
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "cap00001",
    terminals,
    userLabels: {},
    labels: {},
    claudeTitles: {},
    poppedOutTabs: [],
  });
  useCaptain.setState({
    captainIds: ["cap00001", "cap00002"],
    claims: { cap00001: claim("cap00001", ["crewrun0"]) },
    activeCaptainId: "cap00001",
    orchestratorId: null,
    open: false,
    anchorMenuOpen: false,
    deckOpen: true,
  });
  useSupervision.setState({
    trees: {},
    statuses: { "sess-run": "working" },
    snapshots: {},
    sessionIdByTmux: { th_crewrun0: "sess-run" },
  });
});

describe("CaptainsDeck", () => {
  it("tiles every pinned captain (MRU order)", () => {
    render(<CaptainsDeck />);
    const tiles = [...document.querySelectorAll("[data-deck-tile]")].map((t) =>
      t.getAttribute("data-deck-tile"),
    );
    expect(tiles).toEqual(["cap00001", "cap00002"]);
  });

  it("shows the STABLE identity (workspace tab name), never the volatile Claude title", () => {
    // cap00002 has a junk Claude title but no rename: the tile must show its
    // workspace tab name "Backend", not "task notification".
    act(() => {
      useWorkspace.getState().setClaudeTitle("cap00002", "task notification");
    });
    render(<CaptainsDeck />);
    expect(tile("cap00002").textContent).toContain("Backend");
    expect(tile("cap00002").textContent).not.toContain("task notification");
  });

  it("renders the real crew summary from the registry", () => {
    render(<CaptainsDeck />);
    // cap00001 has one crew (crewrun0, working -> running).
    expect(tile("cap00001").textContent).toContain("crew: 1 running · 0 done");
    // cap00002 has no claim -> no crew line.
    expect(tile("cap00002").textContent).not.toContain("crew:");
  });

  it("marks the designated orchestrator tile", () => {
    act(() => {
      useCaptain.getState().setOrchestratorId("cap00002");
    });
    render(<CaptainsDeck />);
    expect(tile("cap00002").getAttribute("data-orchestrator")).toBe("true");
    expect(tile("cap00002").textContent).toContain("orchestrator");
    expect(tile("cap00001").getAttribute("data-orchestrator")).toBeNull();
  });

  it("clicking a tile summons that captain and closes the deck", () => {
    render(<CaptainsDeck />);
    fireEvent.click(tile("cap00002"));
    const s = useCaptain.getState();
    expect(s.deckOpen).toBe(false);
    expect(s.activeCaptainId).toBe("cap00002");
    expect(s.open).toBe(true);
  });

  it("shows the empty state when no captains are pinned", () => {
    act(() => {
      useCaptain.setState({ captainIds: [], activeCaptainId: null });
    });
    render(<CaptainsDeck />);
    expect(document.querySelectorAll("[data-deck-tile]")).toHaveLength(0);
    expect(document.body.textContent).toContain("No captains pinned yet");
  });
});
