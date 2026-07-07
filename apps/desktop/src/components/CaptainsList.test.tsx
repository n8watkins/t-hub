// Tests for the sidebar CAPTAINS section body (captain-sidebar PRD slice B on
// top of the captain-rows round): row render from seeded stores (workspace-first
// ordering, identity line, controlling workspaces + REAL crew summary, tasks
// badge, context meter), the inline rename round-trip through setTerminalLabel,
// summon wiring through the captain store, the attention roll-up precedence (the
// captain's OR a crewmate's needs-input pulses; working and the rate-limit
// overlay do NOT pulse the row - rate-limit reads amber on the meter instead),
// and the crewmate sub-rows + inline SupervisionTree expansion.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";

// CaptainsList -> CaptainOverlay (status dot + display label) -> TerminalPool
// -> xterm, whose import-time color math needs a real <canvas>. Nothing here
// renders a terminal, so stub the pool to keep xterm out of jsdom entirely.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { CaptainsList } from "./CaptainsList";
import { useCaptain, type CaptainClaimRecord } from "../store/captain";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  type WorkspaceTab,
} from "../store/workspace";
import { useSupervision } from "../store/supervision";
import type { SessionStatus, StatusSnapshot, SupervisionTree } from "../ipc/model";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

function claim(
  id: string,
  workspaceTabIds: string[],
  crew: string[],
): CaptainClaimRecord {
  return { captainSessionId: id, shipSlug: `ship-${id}`, workspaceTabIds, crew };
}

/** Two pinned captains: cap00001 (MRU front / active, with a bound session and
 *  two crew - one mid-turn, one done) controlling Workspace 1; bbb00001 (no
 *  session, no claim yet) whose tile lives in Workspace 2. */
beforeEach(() => {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["cap00001", "crewrun0", "crewdon0"] },
    { id: "t2", name: "Workspace 2", order: ["bbb00001"] },
  ];
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "cap00001",
    terminals: {
      // cap00001's cwd basename "monorepo-app" is its stable identity - it must
      // win over the tab name "Workspace 1" (a grouping, not an identity).
      cap00001: term("cap00001", "/home/n/appturnity/monorepo-app"),
      bbb00001: term("bbb00001"),
      crewrun0: term("crewrun0"),
      crewdon0: term("crewdon0"),
    },
    poppedOutTabs: [],
    // Reset renames between cases (setTerminalLabel writes these maps).
    userLabels: {},
    labels: {},
  });
  useCaptain.setState({
    captainIds: ["cap00001", "bbb00001"],
    claims: { cap00001: claim("cap00001", ["t1"], ["crewrun0", "crewdon0"]) },
    activeCaptainId: "cap00001",
    open: false,
    anchorMenuOpen: false,
  });
  const tree: SupervisionTree = {
    sessionId: "sess-1",
    status: "working",
    children: [
      { parentSessionId: "sess-1", agentId: "agent-run", state: "running", startedAt: 1 },
      {
        parentSessionId: "sess-1",
        agentId: "agent-done",
        state: "completed",
        startedAt: 1,
        endedAt: 5,
      },
    ],
    outstandingTasks: 2,
  };
  const snap: StatusSnapshot = {
    sessionId: "sess-1",
    contextUsedPct: 42,
    rateLimitsPresent: false,
    ingestedAtMs: 1,
    tmuxSession: "th_cap00001",
  };
  useSupervision.setState({
    trees: { "sess-1": tree },
    // The captain works; one crew mid-turn, one crew completed.
    statuses: { "sess-1": "working", "sess-run": "working", "sess-don": "completed" },
    snapshots: { "sess-1": snap },
    sessionIdByTmux: {
      th_cap00001: "sess-1",
      th_crewrun0: "sess-run",
      th_crewdon0: "sess-don",
    },
  });
});

function row(terminalId: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(
    `[data-captain-row="${terminalId}"]`,
  );
  expect(el).toBeTruthy();
  return el!;
}

function setStatus(sessionId: string, status: SessionStatus): void {
  act(() => {
    useSupervision.getState().setStatus(sessionId, status);
  });
}

describe("CaptainsList render", () => {
  it("renders one row per pinned captain in MRU order with controlling workspace + crew summary", () => {
    render(<CaptainsList />);
    const rows = [...document.querySelectorAll("[data-captain-row]")].map((r) =>
      r.getAttribute("data-captain-row"),
    );
    expect(rows).toEqual(["cap00001", "bbb00001"]);

    // The claim-bound captain: STABLE identity (no rename -> the cwd basename
    // "monorepo-app", NOT the tab name), plus its controlling workspace
    // "Workspace 1" from workspaceTabIds, REAL crew counts, tasks badge, meter.
    const capRow = row("cap00001");
    expect(capRow.textContent).toContain("monorepo-app"); // identity (cwd basename)
    expect(capRow.textContent).toContain("Workspace 1"); // controlling workspace line
    expect(capRow.textContent).toContain("crew: 1 running · 1 done");
    expect(within(capRow).getByTitle(/outstanding background task/).textContent).toBe(
      "2 tasks",
    );
    expect(
      within(capRow).getByLabelText(/Context window 42 percent full/),
    ).toBeTruthy();

    // The claim-less captain: identity + workspace only - no invented crew.
    const bbbRow = row("bbb00001");
    expect(bbbRow.textContent).toContain("Workspace 2");
    expect(bbbRow.textContent).not.toContain("crew:");
    expect(bbbRow.textContent).not.toContain("task");
  });

  it("dims a captain whose tile is gone (tab popped out)", () => {
    // Drop bbb00001's tile: its tab no longer lists it, and it has no claim.
    act(() => {
      useWorkspace.setState({
        tabs: [
          { id: "t1", name: "Workspace 1", order: ["cap00001", "crewrun0", "crewdon0"] },
          { id: "t2", name: "Workspace 2", order: [] },
        ],
      });
    });
    render(<CaptainsList />);
    const summon = within(row("bbb00001")).getByTitle(/terminal not available/);
    expect(summon).toBeTruthy();
    expect(row("bbb00001").textContent).toContain("tile not available");
  });
});

describe("CaptainsList rename", () => {
  function startRename(terminalId: string): HTMLInputElement {
    fireEvent.click(within(row(terminalId)).getByTitle("Rename captain"));
    return within(row(terminalId)).getByRole("textbox") as HTMLInputElement;
  }

  it("pencil -> type -> Enter commits through setTerminalLabel (persisted rename)", () => {
    render(<CaptainsList />);
    const input = startRename("cap00001");
    // Draft seeds from the CURRENT override (none yet), placeholder shows the
    // derived STABLE identity (the cwd basename, no rename set).
    expect(input.value).toBe("");
    expect(input.placeholder).toBe("monorepo-app");
    fireEvent.change(input, { target: { value: "Flagship" } });
    fireEvent.keyDown(input, { key: "Enter" });
    // Round-trip: the store carries the rename and the row leads with it.
    expect(useWorkspace.getState().userLabels["cap00001"]).toBe("Flagship");
    expect(row("cap00001").textContent).toContain("Flagship");
    expect(within(row("cap00001")).queryByRole("textbox")).toBeNull();
  });

  it("Esc cancels without touching the store", () => {
    render(<CaptainsList />);
    const input = startRename("cap00001");
    fireEvent.change(input, { target: { value: "Nope" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(useWorkspace.getState().userLabels["cap00001"]).toBeUndefined();
    expect(row("cap00001").textContent).not.toContain("Nope");
    expect(within(row("cap00001")).queryByRole("textbox")).toBeNull();
  });

  it("blur commits the draft (click-away does not lose the rename)", () => {
    render(<CaptainsList />);
    const input = startRename("cap00001");
    fireEvent.change(input, { target: { value: "Blurred" } });
    fireEvent.blur(input);
    expect(useWorkspace.getState().userLabels["cap00001"]).toBe("Blurred");
    expect(within(row("cap00001")).queryByRole("textbox")).toBeNull();
  });

  it("committing an emptied draft clears the override back to the derived identity", () => {
    act(() => {
      useWorkspace.getState().setTerminalLabel("cap00001", "Flagship");
    });
    render(<CaptainsList />);
    expect(row("cap00001").textContent).toContain("Flagship");
    const input = startRename("cap00001");
    expect(input.value).toBe("Flagship"); // seeded with the current override
    fireEvent.change(input, { target: { value: "" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(useWorkspace.getState().userLabels["cap00001"]).toBeUndefined();
    // Cleared rename reverts to the stable identity (the workspace tab name).
    expect(row("cap00001").textContent).toContain("Workspace 1");
  });
});

describe("CaptainsList workspace-relevant ordering", () => {
  it("floats the ACTIVE workspace's captain to the top with the accent marker", () => {
    render(<CaptainsList />);
    // Active tab t1 -> cap00001 (already MRU front) leads and is marked.
    let rows = [...document.querySelectorAll("[data-captain-row]")].map((r) =>
      r.getAttribute("data-captain-row"),
    );
    expect(rows).toEqual(["cap00001", "bbb00001"]);
    expect(
      row("cap00001").querySelector("[data-in-active-workspace]"),
    ).toBeTruthy();
    expect(
      row("bbb00001").querySelector("[data-in-active-workspace]"),
    ).toBeNull();

    // Switch to Workspace 2: bbb00001 floats to the top and takes the marker,
    // even though cap00001 is still the MRU-front / active captain.
    act(() => {
      useWorkspace.setState({ activeTabId: "t2" });
    });
    rows = [...document.querySelectorAll("[data-captain-row]")].map((r) =>
      r.getAttribute("data-captain-row"),
    );
    expect(rows).toEqual(["bbb00001", "cap00001"]);
    expect(
      row("bbb00001").querySelector("[data-in-active-workspace]"),
    ).toBeTruthy();
    expect(
      row("cap00001").querySelector("[data-in-active-workspace]"),
    ).toBeNull();
  });
});

describe("CaptainsList click-to-focus wiring", () => {
  it("clicking a row navigates to the Captains tab and focuses that agent's tile", () => {
    render(<CaptainsList />);
    fireEvent.click(within(row("bbb00001")).getByTitle(/Open in Captains/));
    const ws = useWorkspace.getState();
    // The reserved Captains tab is now active and the agent's tile is focused.
    expect(ws.activeTabId).toBe(CAPTAINS_TAB_ID);
    expect(ws.focusedId).toBe("bbb00001");
    // The floating overlay is NOT opened by a row click.
    expect(useCaptain.getState().open).toBe(false);
  });
});

describe("CaptainsList attention roll-up", () => {
  it("pulses on the captain's own needsPermission and needsQuestion (with an aria status)", () => {
    render(<CaptainsList />);
    expect(row("cap00001").querySelector("[data-attention]")).toBeNull();
    setStatus("sess-1", "needsPermission");
    expect(row("cap00001").querySelector("[data-attention]")).toBeTruthy();
    // The pulse is not color-only: a role="status" sibling announces it.
    expect(
      within(row("cap00001")).getByRole("status").textContent,
    ).toContain("needs attention");
    setStatus("sess-1", "needsQuestion");
    expect(row("cap00001").querySelector("[data-attention]")).toBeTruthy();
    expect(within(row("cap00001")).getByRole("status")).toBeTruthy();
  });

  it("pulses the captain row when a CREW session needs input (slice B roll-up)", () => {
    render(<CaptainsList />);
    expect(row("cap00001").querySelector("[data-attention]")).toBeNull();
    // A crewmate hits a permission prompt: it bubbles amber onto its captain.
    setStatus("sess-run", "needsPermission");
    expect(row("cap00001").querySelector("[data-attention]")).toBeTruthy();
  });

  it("does not pulse for working, and the rate-limit overlay warms the meter instead", () => {
    render(<CaptainsList />);
    setStatus("sess-1", "working");
    expect(row("cap00001").querySelector("[data-attention]")).toBeNull();
    // Rate-limit overlay: working + a window at/over the threshold displays as
    // rateLimited - the ROW stays calm (no pulse), the METER flags it.
    act(() => {
      useSupervision.getState().setSnapshot({
        sessionId: "sess-1",
        contextUsedPct: 42,
        rateLimitsPresent: true,
        fiveHour: { usedPercentage: 95 },
        ingestedAtMs: 2,
        tmuxSession: "th_cap00001",
      });
    });
    expect(row("cap00001").querySelector("[data-attention]")).toBeNull();
    expect(
      within(row("cap00001")).getByLabelText(/rate limit near cap/),
    ).toBeTruthy();
  });
});

describe("CaptainsList expansion (crew + subagent tree)", () => {
  it("chevron expands crewmate sub-rows AND the captain's SupervisionTree", () => {
    render(<CaptainsList />);
    fireEvent.click(within(row("cap00001")).getByLabelText(/Expand crew and subagents/));
    // Crewmate sub-rows: one per registry crew id.
    const crewRows = [...document.querySelectorAll("[data-crew-row]")].map((r) =>
      r.getAttribute("data-crew-row"),
    );
    expect(crewRows).toEqual(["crewrun0", "crewdon0"]);
    // The captain's own subagent tree still renders below (distinct from crew).
    expect(screen.getByTitle("running subagents").textContent).toBe("1 running");
    expect(screen.getByTitle("finished subagents").textContent).toBe("1 done");
  });

  it("shows the muted subagent hint for a captain with no session/crew yet", () => {
    render(<CaptainsList />);
    fireEvent.click(within(row("bbb00001")).getByLabelText(/Expand crew and subagents/));
    expect(document.querySelectorAll("[data-crew-row]")).toHaveLength(0);
    expect(
      screen.getByText(/No subagent activity for this session yet/),
    ).toBeTruthy();
  });
});
