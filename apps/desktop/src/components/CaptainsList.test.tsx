// Tests for the sidebar CAPTAINS section body (captain-sidebar PRD, slice A):
// row render from seeded stores (MRU order, workspace name, honest subagent
// summary, tasks badge, context meter), summon wiring through the captain
// store, the attention roll-up precedence (needs-input pulses; working and the
// rate-limit overlay do NOT pulse the row - rate-limit reads amber on the
// meter instead), and the inline SupervisionTree expansion.
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
import { useCaptain } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { useSupervision } from "../store/supervision";
import type { SessionStatus, StatusSnapshot, SupervisionTree } from "../ipc/model";
import type { TerminalInfo } from "../ipc/types";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

/** Two pinned captains: cap00001 (MRU front / active, with a bound session)
 *  in Workspace 1, bbb00001 (no session yet) in Workspace 2. */
beforeEach(() => {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["cap00001"] },
    { id: "t2", name: "Workspace 2", order: ["bbb00001"] },
  ];
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "cap00001",
    terminals: { cap00001: term("cap00001"), bbb00001: term("bbb00001") },
    poppedOutTabs: [],
  });
  useCaptain.setState({
    captainIds: ["cap00001", "bbb00001"],
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
    statuses: { "sess-1": "working" },
    snapshots: { "sess-1": snap },
    sessionIdByTmux: { th_cap00001: "sess-1" },
  });
});

function row(terminalId: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(
    `[data-captain-row="${terminalId}"]`,
  );
  expect(el).toBeTruthy();
  return el!;
}

function setStatus(status: SessionStatus): void {
  act(() => {
    useSupervision.getState().setStatus("sess-1", status);
  });
}

describe("CaptainsList render", () => {
  it("renders one row per pinned captain in MRU order with workspace + summary", () => {
    render(<CaptainsList />);
    const rows = [...document.querySelectorAll("[data-captain-row]")].map((r) =>
      r.getAttribute("data-captain-row"),
    );
    expect(rows).toEqual(["cap00001", "bbb00001"]);

    // The session-bound captain: workspace, honest subagent counts, tasks
    // badge, context meter from the snapshot.
    const capRow = row("cap00001");
    expect(capRow.textContent).toContain("Workspace 1");
    expect(capRow.textContent).toContain("subagents: 1 running · 1 done");
    expect(within(capRow).getByTitle(/outstanding background task/).textContent).toBe(
      "2 tasks",
    );
    expect(
      within(capRow).getByLabelText(/Context window 42 percent full/),
    ).toBeTruthy();

    // The session-less captain: workspace only - no invented activity.
    const bbbRow = row("bbb00001");
    expect(bbbRow.textContent).toContain("Workspace 2");
    expect(bbbRow.textContent).not.toContain("subagents:");
    expect(bbbRow.textContent).not.toContain("task");
  });

  it("dims a captain whose tile is gone (tab popped out)", () => {
    // Drop bbb00001's tile: its tab no longer lists it.
    act(() => {
      useWorkspace.setState({
        tabs: [
          { id: "t1", name: "Workspace 1", order: ["cap00001"] },
          { id: "t2", name: "Workspace 2", order: [] },
        ],
      });
    });
    render(<CaptainsList />);
    const summon = within(row("bbb00001")).getByTitle(/tile not available/);
    expect(summon).toBeTruthy();
    expect(row("bbb00001").textContent).toContain("tile not available");
  });
});

describe("CaptainsList summon wiring", () => {
  it("clicking a row summons that captain (overlay open, MRU front)", () => {
    render(<CaptainsList />);
    fireEvent.click(within(row("bbb00001")).getByTitle(/Summon captain/));
    const s = useCaptain.getState();
    expect(s.open).toBe(true);
    expect(s.activeCaptainId).toBe("bbb00001");
    expect(s.captainIds[0]).toBe("bbb00001");
  });
});

describe("CaptainsList attention roll-up", () => {
  it("pulses on needsPermission and needsQuestion (with an aria status)", () => {
    render(<CaptainsList />);
    expect(row("cap00001").querySelector("[data-attention]")).toBeNull();
    setStatus("needsPermission");
    expect(row("cap00001").querySelector("[data-attention]")).toBeTruthy();
    // The pulse is not color-only: a role="status" sibling announces it.
    expect(
      within(row("cap00001")).getByRole("status").textContent,
    ).toContain("needs attention");
    setStatus("needsQuestion");
    expect(row("cap00001").querySelector("[data-attention]")).toBeTruthy();
    expect(within(row("cap00001")).getByRole("status")).toBeTruthy();
  });

  it("does not pulse for working, and the rate-limit overlay warms the meter instead", () => {
    render(<CaptainsList />);
    setStatus("working");
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

describe("CaptainsList inline supervision tree", () => {
  it("chevron expands the SupervisionTree for the bound session", () => {
    render(<CaptainsList />);
    fireEvent.click(
      within(row("cap00001")).getByLabelText(/Expand subagent activity/),
    );
    // The tree body's counts line (distinct from the row's summary line).
    expect(screen.getByTitle("running subagents").textContent).toBe("1 running");
    expect(screen.getByTitle("finished subagents").textContent).toBe("1 done");
  });

  it("shows the muted hint for a captain with no session yet", () => {
    render(<CaptainsList />);
    fireEvent.click(
      within(row("bbb00001")).getByLabelText(/Expand subagent activity/),
    );
    expect(
      screen.getByText(/No subagent activity for this session yet/),
    ).toBeTruthy();
  });
});
