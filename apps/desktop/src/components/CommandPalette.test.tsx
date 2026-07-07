// The command palette's "Summon captain" entries must use the STABLE identity
// (user rename -> cwd basename -> workspace tab name), never the volatile Claude
// session title - this was the one surface the deck's identity fix missed.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, fireEvent, render } from "@testing-library/react";

// CommandPalette -> CaptainOverlay (stableCaptainIdentity) -> TerminalPool ->
// xterm; stub the pool so xterm's canvas math stays out of jsdom.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { CommandPalette, openKeyboardPalette } from "./CommandPalette";
import { useCaptain } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

// jsdom does not implement scrollIntoView, which the palette calls on selection.
Element.prototype.scrollIntoView = vi.fn();

beforeEach(() => {
  localStorage.clear();
  const tabs: WorkspaceTab[] = [{ id: "t1", name: "Backend", order: ["cap00001"] }];
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "cap00001",
    terminals: { cap00001: term("cap00001", "/home/n/appturnity/monorepo-app") },
    userLabels: {},
    labels: {},
    claudeTitles: {},
    poppedOutTabs: [],
  });
  useCaptain.setState({
    captainIds: ["cap00001"],
    activeCaptainId: "cap00001",
    claims: {},
    open: false,
    anchorMenuOpen: false,
    deckOpen: false,
  });
});

describe("CommandPalette Summon captain identity", () => {
  it("uses the STABLE identity (cwd basename), never the tab name or Claude title", () => {
    // cap00001 has a junk Claude title and no rename, and its tab "Backend" is
    // a grouping - the entry must read the cwd basename "monorepo-app", not the
    // tab name and not "task notification".
    act(() => useWorkspace.getState().setClaudeTitle("cap00001", "task notification"));
    act(() => openKeyboardPalette());
    render(<CommandPalette />);
    // Surface the dynamic captain entry by searching for it.
    const input = document.querySelector<HTMLInputElement>("input")!;
    fireEvent.change(input, { target: { value: "Summon captain" } });

    expect(document.body.textContent).toContain("Summon captain: monorepo-app");
    expect(document.body.textContent).not.toContain("Summon captain: Backend");
    expect(document.body.textContent).not.toContain("task notification");
  });

  it("prefers the user rename over the cwd basename", () => {
    act(() => {
      useWorkspace.getState().setTerminalLabel("cap00001", "Flagship");
      useWorkspace.getState().setClaudeTitle("cap00001", "task notification");
    });
    act(() => openKeyboardPalette());
    render(<CommandPalette />);
    const input = document.querySelector<HTMLInputElement>("input")!;
    fireEvent.change(input, { target: { value: "Summon captain" } });

    expect(document.body.textContent).toContain("Summon captain: Flagship");
  });
});
