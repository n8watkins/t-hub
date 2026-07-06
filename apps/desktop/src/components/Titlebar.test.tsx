// Regression tests for the captain anchor dropdown (the "anchor button does
// nothing" bug): App.tsx's default titlebar wrapper is a height:TITLEBAR_H,
// overflow:hidden box (the auto-hide height animation needs the clip), so a
// dropdown rendered INSIDE the titlebar subtree below the 32px row was 100%
// clipped - the click actually worked (anchorMenuOpen flipped true) but the
// menu was invisible, while its fixed backdrop ESCAPED the clip and swallowed
// the next pointerdown, making the button feel inert. The fix portals the
// menu + backdrop to document.body; these tests pin that contract plus the
// existing close paths (Esc via lib/escOverlays, backdrop pointerdown).
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

// escOverlays writes the Shift+Esc passthrough byte via the IPC client; stub
// the module so no Tauri invoke path is reachable under jsdom (mirrors
// escOverlays.test.ts).
vi.mock("../ipc/client", () => ({
  writeTerminal: vi.fn(() => Promise.resolve()),
}));

// Titlebar -> CaptainOverlay (status dot + display label) -> TerminalPool ->
// xterm, whose import-time color math needs a real <canvas>. Nothing here
// renders a terminal, so stub the pool to keep xterm out of jsdom entirely.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { Titlebar } from "./Titlebar";
import { useCaptain } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import type { TerminalInfo } from "../ipc/types";
import { handleOverlayEscape } from "../lib/escOverlays";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

beforeEach(() => {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["cap00001"] },
  ];
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "cap00001",
    terminals: { cap00001: term("cap00001") },
    poppedOutTabs: [],
  });
  useCaptain.setState({
    captainIds: ["cap00001"],
    activeCaptainId: "cap00001",
    open: false,
    anchorMenuOpen: false,
  });
});

/** Render the main-window titlebar and open the anchor menu via a real click
 *  (the end-user path that looked broken). Returns the titlebar's container
 *  so tests can assert the menu escaped its subtree. */
function openMenu(): HTMLElement {
  const { container } = render(<Titlebar />);
  fireEvent.click(screen.getByRole("button", { name: "Captain menu" }));
  expect(useCaptain.getState().anchorMenuOpen).toBe(true);
  return container;
}

describe("captain anchor dropdown (portal)", () => {
  it("mounts the menu under document.body, OUTSIDE the titlebar subtree", () => {
    const container = openMenu();
    const menu = screen.getByRole("menu", { name: "Pinned captains" });
    // The regression: inside the titlebar subtree the menu sits entirely below
    // the 32px overflow-hidden wrapper row and is fully clipped.
    expect(container.contains(menu)).toBe(false);
    expect(document.body.contains(menu)).toBe(true);
    // The pinned captain's row rendered and is summonable.
    expect(screen.getAllByRole("menuitem")).toHaveLength(1);
  });

  it("closes via handleOverlayEscape (the single Esc dispatch point)", () => {
    openMenu();
    act(() => {
      expect(handleOverlayEscape(false)).toBe(true);
    });
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(screen.queryByRole("menu", { name: "Pinned captains" })).toBeNull();
  });

  it("closes on backdrop pointerdown (the backdrop is portaled too)", () => {
    const container = openMenu();
    const menu = screen.getByRole("menu", { name: "Pinned captains" });
    // The backdrop is the menu's immediately-preceding portal sibling.
    const backdrop = menu.previousElementSibling as HTMLElement;
    expect(backdrop).toBeTruthy();
    expect(container.contains(backdrop)).toBe(false);
    fireEvent.pointerDown(backdrop);
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(screen.queryByRole("menu", { name: "Pinned captains" })).toBeNull();
  });

  it("summons the clicked captain and closes the menu", () => {
    openMenu();
    fireEvent.click(screen.getAllByRole("menuitem")[0]);
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(useCaptain.getState().open).toBe(true);
    expect(useCaptain.getState().activeCaptainId).toBe("cap00001");
  });
});
