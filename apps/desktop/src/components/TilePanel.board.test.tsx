// Tests for the Board tab wiring (POWDER-1): the "board" panel tab reuses the
// shared WebPreview surface, seeds its URL from the per-tile last-committed
// address falling back to the configured settings.powderBoardUrl, and persists
// a committed navigation back into usePanels.boardUrl so it survives a tab
// switch. WebPreview itself is stubbed to a prop-capturing shim (its iframe /
// watchdog behavior is exercised elsewhere and needs a real browser); these
// tests only assert the plumbing between TilePanel, the panels store, and the
// settings store.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";

// Capture the props TilePanel hands WebPreview, and expose a button that fires
// its onNavigate so we can assert the committed URL is persisted.
const webPreviewProps: { initialUrl?: string; onNavigate?: (u: string) => void }[] = [];
vi.mock("./WebPreview", () => ({
  WebPreview: (props: { initialUrl?: string; onNavigate?: (u: string) => void }) => {
    webPreviewProps.push(props);
    return (
      <div>
        <span data-testid="initial-url">{props.initialUrl ?? ""}</span>
        <button
          type="button"
          onClick={() => props.onNavigate?.("https://board.example.ts.net")}
        >
          navigate
        </button>
      </div>
    );
  },
}));
// FilePanel / DevTab aren't rendered by these board-tab cases, but stub them so
// the module graph never pulls xterm / file-tree deps into jsdom.
vi.mock("./FilePanel", () => ({ FilePanel: () => null }));
vi.mock("./DevTab", () => ({ DevTab: () => null }));

import { TilePanel } from "./TilePanel";
import { usePanels } from "../store/panels";
import { useSettings } from "../store/settings";

beforeEach(() => {
  webPreviewProps.length = 0;
  // Reset the per-tile URL slots and set a known configured default.
  usePanels.setState({ boardUrl: {}, previewUrl: {}, devUrl: {} });
  useSettings.setState({ powderBoardUrl: "http://localhost:4000" });
});

describe("TilePanel board tab", () => {
  it("seeds the board URL from the configured powderBoardUrl default", () => {
    const { getByTestId } = render(
      <TilePanel terminalId="t1" cwd="/tmp/p" tab="board" />,
    );
    expect(getByTestId("initial-url").textContent).toBe("http://localhost:4000");
  });

  it("prefers the per-tile committed board URL over the default", () => {
    usePanels.setState({ boardUrl: { t1: "https://saved.example.ts.net" } });
    const { getByTestId } = render(
      <TilePanel terminalId="t1" cwd="/tmp/p" tab="board" />,
    );
    expect(getByTestId("initial-url").textContent).toBe(
      "https://saved.example.ts.net",
    );
  });

  it("persists a committed navigation into usePanels.boardUrl (survives a tab switch)", () => {
    const { getByText } = render(
      <TilePanel terminalId="t1" cwd="/tmp/p" tab="board" />,
    );
    fireEvent.click(getByText("navigate"));
    expect(usePanels.getState().boardUrl.t1).toBe("https://board.example.ts.net");
  });

  it("keeps each tile's board URL independent", () => {
    usePanels.setState({ boardUrl: { t1: "https://one.ts.net" } });
    const { getByTestId } = render(
      <TilePanel terminalId="t2" cwd="/tmp/p" tab="board" />,
    );
    // t2 has no saved URL -> falls back to the configured default, not t1's.
    expect(getByTestId("initial-url").textContent).toBe("http://localhost:4000");
  });
});
