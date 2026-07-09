// Tests for the tile HEADER's orchestrator identity (the pane counterpart of
// the sidebar's OrchestratorRow): the designated orchestrator's tile reads the
// fixed brand name "Cortana" with the crown badge in place of the derived cwd
// basename ("orchestrator"), a plain tile keeps its folder name, and the
// substitution tracks the designation live (display-only - the store's
// orchestratorId is the single input).
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, fireEvent, render, within } from "@testing-library/react";

// Tile -> TerminalPool -> xterm, whose import-time color math needs a real
// <canvas>; and Tile -> TilePanel -> Files/Preview surfaces. Nothing here
// renders a terminal body, so stub both to keep xterm out of jsdom entirely
// (the CaptainsList.test.tsx pattern).
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));
vi.mock("./TilePanel", () => ({
  TilePanel: () => null,
}));
// The header's git chip polls the control socket on mount - stub it PENDING
// forever so the chip simply never renders (a resolved value would setState
// after render and trip React's act() warning; the header under test doesn't
// need the chip).
vi.mock("../ipc/git", () => ({
  gitInfo: vi.fn(() => new Promise(() => {})),
}));

import { Tile } from "./Tile";
import { useCaptain } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

/** Two live tiles: orch0001 in the canonical orchestrator home (its cwd
 *  basename - the derived header name - would read "orchestrator") and
 *  cap00001 in an ordinary project folder. No designation yet per case. */
beforeEach(() => {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["orch0001", "cap00001"] },
  ];
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "orch0001",
    terminals: {
      orch0001: term("orch0001", "/home/n/.t-hub/orchestrator"),
      cap00001: term("cap00001", "/home/n/appturnity/monorepo-app"),
    },
    poppedOutTabs: [],
    userLabels: {},
    labels: {},
  });
  useCaptain.setState({ orchestratorId: null, captainIds: [] });
});

function renderTile(terminalId: string): HTMLElement {
  render(
    <Tile
      terminalId={terminalId}
      focused={false}
      onFocus={() => {}}
      onClose={() => {}}
    />,
  );
  const header = document.querySelector<HTMLElement>(
    `[data-tile-id="${terminalId}"] .th-tile-header`,
  );
  expect(header).toBeTruthy();
  return header!;
}

describe("Tile header orchestrator identity", () => {
  it("renders Cortana + the crown on the designated orchestrator's header, not the cwd basename", () => {
    useCaptain.setState({ orchestratorId: "orch0001" });
    const header = renderTile("orch0001");
    // The fixed brand name replaces the derived basename entirely.
    expect(header.textContent).toContain("Cortana");
    expect(header.textContent).not.toContain("orchestrator");
    // The crown badge (the sidebar's marker, accent-colored) is present.
    expect(within(header).getByLabelText("Orchestrator")).toBeTruthy();
  });

  it("keeps the plain folder basename (no crown) on a non-orchestrator tile", () => {
    useCaptain.setState({ orchestratorId: "orch0001" });
    const header = renderTile("cap00001");
    expect(header.textContent).toContain("monorepo-app");
    expect(header.textContent).not.toContain("Cortana");
    expect(within(header).queryByLabelText("Orchestrator")).toBeNull();
  });

  it("tracks the designation live: derived name until marked, Cortana after", () => {
    const header = renderTile("orch0001");
    // Undesignated: the header shows the honest derived basename.
    expect(header.textContent).toContain("orchestrator");
    expect(within(header).queryByLabelText("Orchestrator")).toBeNull();
    // Designate through the store (what the tile context-menu action calls).
    act(() => {
      useCaptain.getState().setOrchestratorId("orch0001");
    });
    expect(header.textContent).toContain("Cortana");
    expect(header.textContent).not.toContain("orchestrator");
    expect(within(header).getByLabelText("Orchestrator")).toBeTruthy();
  });
});

describe("Tile kill+restart confirm (captain de-captain warning)", () => {
  /** Click the header's kill+restart control and return the open confirm dialog. */
  function openRestartConfirm(terminalId: string): HTMLElement {
    const header = renderTile(terminalId);
    act(() => {
      fireEvent.click(
        within(header).getByLabelText("Kill and restart session"),
      );
    });
    const dialog = document.querySelector<HTMLElement>('[role="alertdialog"]');
    expect(dialog).toBeTruthy();
    return dialog!;
  }

  it("warns that a CAPTAIN tile will be de-captained and its crew detached", () => {
    useCaptain.setState({ orchestratorId: null, captainIds: ["cap00001"] });
    const dialog = openRestartConfirm("cap00001");
    expect(dialog.textContent).toContain("a captain");
    expect(dialog.textContent).toContain("de-captain the ship");
    expect(dialog.textContent).toContain("detach its crew");
  });

  it("names the ORCHESTRATOR specifically in the warning", () => {
    useCaptain.setState({ orchestratorId: "orch0001", captainIds: [] });
    const dialog = openRestartConfirm("orch0001");
    expect(dialog.textContent).toContain("the orchestrator");
    expect(dialog.textContent).toContain("de-captain the ship");
  });

  it("omits the de-captain warning for a plain (non-captain) tile", () => {
    useCaptain.setState({ orchestratorId: null, captainIds: [] });
    const dialog = openRestartConfirm("cap00001");
    expect(dialog.textContent).toContain("recover a frozen terminal");
    expect(dialog.textContent).not.toContain("de-captain");
  });
});
