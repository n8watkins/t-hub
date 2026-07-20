// Sidebar section-layout tests for the captain-sidebar PRD (slice A): the
// CAPTAINS section appears above WORKSPACES only while captains are pinned,
// and the HISTORY body is capped to ~3 rows (RECENT_BODY_MAX_PX) instead of the
// old 38vh wall. Heavy children (recent fetch, usage pollers, WSL telemetry,
// the workspaces list, the terminal pool behind the captain rows) are stubbed
// - this suite pins the SECTION layout, not their internals.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render } from "@testing-library/react";

vi.mock("./HistoryList", () => ({
  HistoryList: () => <div data-testid="history-list" />,
}));
vi.mock("./WorkspacesList", () => ({
  WorkspacesList: () => <div data-testid="workspaces-list" />,
}));
vi.mock("./WslHealth", () => ({
  WslHealth: () => null,
  gib: (n: number) => `${n}`,
  usedFraction: () => 0,
}));
vi.mock("./UsageStrip", () => ({
  UsageStrip: () => null,
  UsageInline: () => null,
  useClaudeUsage: () => null,
  CodexUsageStrip: () => null,
  CodexUsageInline: () => null,
  useCodexUsage: () => null,
}));
vi.mock("../store/telemetry", () => ({
  useAgentTelemetry: () => ({ metrics: null, agent: undefined }),
}));
// CaptainsList -> CaptainOverlay -> TerminalPool -> xterm (canvas): stub the pool.
vi.mock("./TerminalPool", () => ({
  useTerminalSlot: () => ({ current: null }),
  requestPoolSync: () => {},
}));

import { Sidebar, RECENT_BODY_MAX_PX, RECENT_ROW_APPROX_PX } from "./Sidebar";
import { useCaptain } from "../store/captain";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

beforeEach(() => {
  localStorage.clear();
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
    captainIds: [],
    activeCaptainId: null,
    open: false,
    anchorMenuOpen: false,
  });
});

/** Section titles in DOM order. The title span lives inside the uppercase
 *  header wrapper (button when collapsible, div otherwise) - scoped there so a
 *  future span inside a header control can't silently shift the mapping. */
function sectionTitles(container: HTMLElement): string[] {
  return [...container.querySelectorAll("section")].map(
    (s) => s.querySelector(".uppercase span")?.textContent ?? "",
  );
}

describe("Sidebar captains section", () => {
  it("is absent while nothing is pinned", () => {
    const { container } = render(<Sidebar mode="full" />);
    expect(container.textContent).not.toContain("Captains");
  });

  it("appears ABOVE Workspaces when captains are pinned, with rows", () => {
    useCaptain.setState({ captainIds: ["cap00001"], activeCaptainId: "cap00001" });
    const { container } = render(<Sidebar mode="full" />);
    const titles = sectionTitles(container);
    // The section is the AGENTS hierarchy (orchestrator over captains).
    expect(titles[0]).toBe("Captain Workspace");
    expect(titles[1]).toBe("Workspaces");
    expect(container.querySelector('[data-captain-row="cap00001"]')).toBeTruthy();
  });
});

describe("Sidebar History cap", () => {
  it("fits about THREE two-line rows (not fewer, not four)", () => {
    // jsdom has no layout engine, so visible-row-count is asserted as
    // arithmetic against the exported per-row height the cap is derived
    // from: three full rows fit, a fourth cannot. HistoryList rows are
    // TWO-line (13px title + 11px subtitle + py-1.5), which is what sank
    // the first 104px cap (it assumed one-line rows and showed ~2).
    expect(RECENT_BODY_MAX_PX).toBeGreaterThanOrEqual(3 * RECENT_ROW_APPROX_PX);
    expect(RECENT_BODY_MAX_PX).toBeLessThan(4 * RECENT_ROW_APPROX_PX);
  });

  it(`caps the History body at ${RECENT_BODY_MAX_PX}px with internal scroll`, () => {
    const { container } = render(<Sidebar mode="full" />);
    const capped = [...container.querySelectorAll<HTMLElement>("div")].find(
      (d) => d.style.maxHeight === `${RECENT_BODY_MAX_PX}px`,
    );
    expect(capped).toBeTruthy();
    expect(capped!.className).toContain("overflow-y-auto");
    // The capped wrapper is the one holding the History list body.
    expect(capped!.querySelector('[data-testid="history-list"]')).toBeTruthy();
  });
});
