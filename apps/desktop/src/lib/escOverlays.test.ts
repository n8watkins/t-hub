// Unit tests for the single Escape dispatch point (captain-overlay fix round,
// extended for captain-list): explicit anchor-dropdown-vs-overlay-vs-fullscreen
// precedence + the Shift+Esc literal-Esc passthrough to the ACTIVE captain.
import { describe, it, expect, vi, beforeEach } from "vitest";

// escOverlays writes the passthrough byte via the IPC client; stub the whole
// module so no Tauri invoke is attempted under jsdom.
vi.mock("../ipc/client", () => ({
  writeTerminal: vi.fn(() => Promise.resolve()),
}));

import { handleOverlayEscape, overlayEscapeArmed, ESC_BYTE } from "./escOverlays";
import { writeTerminal } from "../ipc/client";
import { useCaptain } from "../store/captain";
import { usePanels } from "../store/panels";
import { useWorkspace } from "../store/workspace";

/** Seed a minimal live workspace: two tabs, captain tile on tab 1, a second
 *  tile focused on tab 2 (the "summoned from another workspace" shape). */
function seedWorkspace(): void {
  useWorkspace.setState({
    tabs: [
      { id: "t1", name: "Workspace 1", order: ["cap00001"] },
      { id: "t2", name: "Workspace 2", order: ["aaa00001"] },
    ],
    activeTabId: "t2",
    focusedId: "aaa00001",
  });
}

beforeEach(() => {
  vi.mocked(writeTerminal).mockClear();
  seedWorkspace();
  usePanels.setState({ fullscreenId: null });
  useCaptain.setState({
    captainIds: ["cap00001"],
    activeCaptainId: "cap00001",
    open: false,
    anchorMenuOpen: false,
    deckOpen: false,
  });
});

describe("handleOverlayEscape precedence", () => {
  it("consumes nothing when neither surface is up", () => {
    expect(handleOverlayEscape(false)).toBe(false);
  });

  it("closes the captains deck FIRST (top-level full-view surface)", () => {
    // Opening the deck already retires the overlay/dropdown/fullscreen, so Esc
    // closes it exclusively - routed here (not the deck's own listener) so the
    // ordering is deterministic.
    useCaptain.getState().setDeckOpen(true);
    expect(useCaptain.getState().deckOpen).toBe(true);
    expect(handleOverlayEscape(false)).toBe(true);
    expect(useCaptain.getState().deckOpen).toBe(false);
    expect(handleOverlayEscape(false)).toBe(false); // nothing left
  });

  it("dismisses the overlay FIRST when overlay + fullscreen are both up", () => {
    useCaptain.getState().openOverlay();
    usePanels.setState({ fullscreenId: "aaa00001" });

    // First Esc: overlay closes, fullscreen untouched.
    expect(handleOverlayEscape(false)).toBe(true);
    expect(useCaptain.getState().open).toBe(false);
    expect(usePanels.getState().fullscreenId).toBe("aaa00001");

    // Second Esc: fullscreen exits.
    expect(handleOverlayEscape(false)).toBe(true);
    expect(usePanels.getState().fullscreenId).toBeNull();

    // Third Esc: nothing left to consume.
    expect(handleOverlayEscape(false)).toBe(false);
  });

  it("exits fullscreen when only fullscreen is up", () => {
    usePanels.setState({ fullscreenId: "aaa00001" });
    expect(handleOverlayEscape(false)).toBe(true);
    expect(usePanels.getState().fullscreenId).toBeNull();
  });

  it("dismisses the anchor dropdown when it is the ONLY surface up", () => {
    // Regression (captain review round 1): with just the dropdown open the
    // listener must still arm and Esc must consume + close it.
    useCaptain.getState().setAnchorMenu(true);
    expect(handleOverlayEscape(false)).toBe(true);
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(handleOverlayEscape(false)).toBe(false); // nothing left
  });

  it("dismisses the titlebar anchor dropdown BEFORE the overlay", () => {
    useCaptain.getState().openOverlay();
    useCaptain.getState().setAnchorMenu(true);

    // First Esc: the dropdown closes, the overlay stays up.
    expect(handleOverlayEscape(false)).toBe(true);
    expect(useCaptain.getState().anchorMenuOpen).toBe(false);
    expect(useCaptain.getState().open).toBe(true);

    // Second Esc: the overlay closes.
    expect(handleOverlayEscape(false)).toBe(true);
    expect(useCaptain.getState().open).toBe(false);
  });
});

describe("overlayEscapeArmed (Canvas listener arming predicate)", () => {
  // The arming condition must cover EVERY surface handleOverlayEscape can
  // consume Esc for - a surface handled but not armed for is a dead Esc key
  // (the round-1 dropdown bug: armed only on fullscreen/overlay, so a
  // dropdown-only Esc never even reached the dispatch).
  const off = {
    fullscreenId: null,
    captainOpen: false,
    anchorMenuOpen: false,
    deckOpen: false,
  };

  it("is disarmed when no surface is up", () => {
    expect(overlayEscapeArmed(off)).toBe(false);
  });

  it("arms for EACH surface alone - including the deck and the anchor dropdown", () => {
    expect(overlayEscapeArmed({ ...off, fullscreenId: "aaa00001" })).toBe(true);
    expect(overlayEscapeArmed({ ...off, captainOpen: true })).toBe(true);
    expect(overlayEscapeArmed({ ...off, anchorMenuOpen: true })).toBe(true);
    expect(overlayEscapeArmed({ ...off, deckOpen: true })).toBe(true);
  });
});

describe("Shift+Esc passthrough", () => {
  it("sends a literal Esc to the captain and keeps the overlay open", () => {
    useCaptain.getState().openOverlay();
    expect(useCaptain.getState().open).toBe(true);

    expect(handleOverlayEscape(true)).toBe(true);
    expect(writeTerminal).toHaveBeenCalledWith("cap00001", ESC_BYTE);
    expect(useCaptain.getState().open).toBe(true); // NOT dismissed
  });

  it("does not write to the terminal when the overlay is closed", () => {
    usePanels.setState({ fullscreenId: "aaa00001" });
    expect(handleOverlayEscape(true)).toBe(true); // falls through to fullscreen
    expect(writeTerminal).not.toHaveBeenCalled();
    expect(usePanels.getState().fullscreenId).toBeNull();
  });

  it("uses the real ESC control byte", () => {
    expect(ESC_BYTE).toBe("\u001b");
  });
});
