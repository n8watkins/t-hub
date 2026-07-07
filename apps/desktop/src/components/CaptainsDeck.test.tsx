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

// Capture the orchestrator input's writes without a real Tauri backend.
const writes: Array<{ id: string; data: string }> = [];
vi.mock("../ipc/client", async (orig) => {
  const actual = await orig<typeof import("../ipc/client")>();
  return {
    ...actual,
    writeTerminal: (id: string, data: string) => {
      writes.push({ id, data });
      return Promise.resolve();
    },
  };
});

import { CaptainsDeck } from "./CaptainsDeck";
import { useCaptain, type CaptainClaimRecord } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { useSupervision } from "../store/supervision";
import type { TerminalInfo } from "../ipc/types";
import {
  registerTerminalTail,
  unregisterTerminalTail,
  type XtermTailSource,
} from "../lib/terminalTail";

/** A fake xterm buffer whose visible rows are `lines` (top -> bottom). */
function fakeTerm(lines: string[]): XtermTailSource {
  return {
    rows: lines.length,
    buffer: {
      active: {
        baseY: 0,
        getLine: (y: number) =>
          lines[y] !== undefined
            ? { translateToString: () => lines[y] }
            : undefined,
      },
    },
  };
}

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

function claim(id: string, crew: string[] = []): CaptainClaimRecord {
  return { captainSessionId: id, shipSlug: `ship-${id}`, workspaceTabIds: [], crew };
}

function panel(terminalId: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(
    `[data-deck-panel="${terminalId}"]`,
  );
  expect(el).toBeTruthy();
  return el!;
}

beforeEach(() => {
  localStorage.clear();
  writes.length = 0;
  unregisterTerminalTail("cap00001");
  unregisterTerminalTail("cap00002");
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

describe("CaptainsDeck agent panels", () => {
  it("renders a live panel per agent, orchestrator FIRST then captains", () => {
    // No orchestrator: panels are the captains in order.
    render(<CaptainsDeck />);
    let panels = [...document.querySelectorAll("[data-deck-panel]")].map((p) =>
      p.getAttribute("data-deck-panel"),
    );
    expect(panels).toEqual(["cap00001", "cap00002"]);

    // Designating cap00002 the orchestrator floats it to the top.
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    panels = [...document.querySelectorAll("[data-deck-panel]")].map((p) =>
      p.getAttribute("data-deck-panel"),
    );
    expect(panels).toEqual(["cap00002", "cap00001"]);
  });

  it("each panel has a LIVE terminal body (the pool placeholder)", () => {
    render(<CaptainsDeck />);
    expect(panel("cap00001").querySelector("[data-deck-terminal]")).toBeTruthy();
  });

  it("shows the STABLE identity (workspace tab name), never the volatile Claude title", () => {
    act(() => {
      useWorkspace.getState().setClaudeTitle("cap00002", "task notification");
    });
    render(<CaptainsDeck />);
    expect(panel("cap00002").textContent).toContain("Backend");
    expect(panel("cap00002").textContent).not.toContain("task notification");
  });

  it("renders the real crew summary in the panel header", () => {
    render(<CaptainsDeck />);
    expect(panel("cap00001").textContent).toContain("crew: 1 running · 0 done");
    expect(panel("cap00002").textContent).not.toContain("crew:");
  });

  it("marks the designated orchestrator panel", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    expect(panel("cap00002").getAttribute("data-orchestrator")).toBe("true");
    expect(panel("cap00002").textContent).toContain("orchestrator");
    expect(panel("cap00001").getAttribute("data-orchestrator")).toBeNull();
  });

  it("clicking a panel FOCUSES it in the deck (stays open, no overlay)", () => {
    render(<CaptainsDeck />);
    fireEvent.click(panel("cap00002"));
    const s = useCaptain.getState();
    expect(s.deckOpen).toBe(true); // stays in the deck
    expect(s.deckFocusId).toBe("cap00002"); // spotlighted
    expect(s.open).toBe(false); // no overlay
    expect(panel("cap00002").getAttribute("data-focused")).toBe("true");
  });

  it("a tile-less agent shows the unavailable affordance (no terminal body)", () => {
    act(() => {
      useWorkspace.setState({
        tabs: [
          { id: "t1", name: "Workspace 1", order: ["cap00001", "crewrun0"] },
          { id: "t2", name: "Backend", order: [] },
        ],
      });
    });
    render(<CaptainsDeck />);
    expect(panel("cap00002").getAttribute("data-tile-available")).toBeNull();
    expect(panel("cap00002").textContent).toContain("Terminal not available");
    expect(panel("cap00002").querySelector("[data-deck-terminal]")).toBeNull();
  });

  it("shows the empty state when there are no agents", () => {
    act(() => {
      useCaptain.setState({ captainIds: [], activeCaptainId: null, orchestratorId: null });
    });
    render(<CaptainsDeck />);
    expect(document.querySelectorAll("[data-deck-panel]")).toHaveLength(0);
    expect(document.body.textContent).toContain("No agents yet");
  });

  it("renders nothing when the deck is closed", () => {
    act(() => useCaptain.setState({ deckOpen: false }));
    render(<CaptainsDeck />);
    expect(document.querySelector("[data-captains-deck]")).toBeNull();
  });
});

describe("CaptainsDeck orchestrator input", () => {
  function field(): HTMLInputElement {
    return document.querySelector<HTMLInputElement>("[data-orchestrator-field]")!;
  }

  it("writes the typed line + carriage return to the designated orchestrator on Enter", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    const input = field();
    expect(input.disabled).toBe(false);
    fireEvent.change(input, { target: { value: "status report please" } });
    fireEvent.submit(input.closest("form")!);
    expect(writes).toEqual([{ id: "cap00002", data: "status report please\r" }]);
    // The input clears after sending.
    expect(field().value).toBe("");
  });

  it("the Send button submits the same way", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    fireEvent.change(field(), { target: { value: "go" } });
    fireEvent.click(document.querySelector("[data-orchestrator-send]")!);
    expect(writes).toEqual([{ id: "cap00002", data: "go\r" }]);
  });

  it("is disabled with a hint when no orchestrator is designated", () => {
    render(<CaptainsDeck />); // orchestratorId null from beforeEach
    const input = field();
    expect(input.disabled).toBe(true);
    expect(input.placeholder).toContain("No orchestrator set");
    fireEvent.change(input, { target: { value: "ignored" } });
    fireEvent.submit(input.closest("form")!);
    expect(writes).toEqual([]);
  });

  it("does not send whitespace-only input", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    fireEvent.change(field(), { target: { value: "   " } });
    fireEvent.submit(field().closest("form")!);
    expect(writes).toEqual([]);
  });

  it("sends the TRIMMED text (leading/trailing whitespace dropped)", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    fireEvent.change(field(), { target: { value: "  hello world  " } });
    fireEvent.submit(field().closest("form")!);
    expect(writes).toEqual([{ id: "cap00002", data: "hello world\r" }]);
  });

  it("offers a disabled Scribe voice placeholder", () => {
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    const mic = document.querySelector<HTMLButtonElement>(
      '[aria-label="Voice input coming via Scribe"]',
    );
    expect(mic).toBeTruthy();
    expect(mic!.disabled).toBe(true);
  });
});

describe("CaptainsDeck orchestrator output strip", () => {
  it("shows the latest visible line of the orchestrator terminal", () => {
    registerTerminalTail("cap00002", fakeTerm(["booting up", "ready > _"]));
    act(() => useCaptain.getState().setOrchestratorId("cap00002"));
    render(<CaptainsDeck />);
    const strip = document.querySelector("[data-orchestrator-strip]");
    expect(strip).toBeTruthy();
    expect(strip!.textContent).toContain("ready > _");
  });

  it("renders no strip until an orchestrator is designated", () => {
    render(<CaptainsDeck />); // orchestratorId null from beforeEach
    expect(document.querySelector("[data-orchestrator-strip]")).toBeNull();
  });
});
