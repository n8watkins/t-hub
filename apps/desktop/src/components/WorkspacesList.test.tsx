// Tests for the bottom WORKSPACES list (fix-captain-workspace-dupes): the
// reserved Captains tab is the AGENTS' home surface - its tiles (the orchestrator
// + pinned captains) are already the top "Agents" list - so it must NEVER render
// as an ordinary workspace row here. Rendering it double-surfaced every captain
// as an unwanted, un-closeable bottom tile (the reserved tab is intentionally
// non-closeable). These pin:
//   1. the reserved Captains tab (and the captain tiles inside it) is absent from
//      the bottom list, while ordinary user workspaces DO render;
//   2. the exclusion survives an adoptRegistry round-trip (the reserved tab is
//      re-appended by adoptRegistry, so a server sync must not resurrect it here);
//   3. an ordinary user workspace still closes normally.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, within } from "@testing-library/react";

// WorkspacesList -> TerminalRow reads the supervision/activity/theme/clientType
// stores but never mounts a terminal; nothing here needs xterm, but popOutTab
// (lib/windows) pulls Tauri window APIs, so stub it to keep the render web-safe.
vi.mock("../lib/windows", () => ({
  popOutTab: () => {},
}));

import { WorkspacesList } from "./WorkspacesList";
import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  type WorkspaceTab,
} from "../store/workspace";
import type { TabReport, TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/tmp"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd, title: id, state: "live" };
}

/** Seed the store with a real work tab plus the always-present reserved Captains
 *  tab (appended LAST, mirroring the live layout) holding two agent tiles. */
function seed(): void {
  const tabs: WorkspaceTab[] = [
    { id: "t1", name: "Workspace 1", order: ["work0001"] },
    {
      id: CAPTAINS_TAB_ID,
      name: CAPTAINS_TAB_NAME,
      order: ["cap00001", "cap00002"],
    },
  ];
  const terminals: Record<string, TerminalInfo> = {};
  for (const t of tabs) for (const id of t.order) terminals[id] = term(id);
  useWorkspace.setState({
    tabs,
    activeTabId: "t1",
    focusedId: "work0001",
    terminals,
    poppedOutTabs: [],
    userLabels: {},
    labels: {},
  });
}

beforeEach(() => {
  localStorage.clear();
  seed();
});

/** The workspace ROW nodes the list rendered, keyed by their tab id. */
function wsRows(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>("[data-th-ws-row]")].map(
    (el) => el.getAttribute("data-th-ws-row") ?? "",
  );
}

describe("WorkspacesList excludes the reserved Captains tab", () => {
  it("renders the work workspace but NOT the reserved Captains tab", () => {
    const { container } = render(<WorkspacesList />);
    const rows = wsRows(container);
    expect(rows).toContain("t1");
    expect(rows).not.toContain(CAPTAINS_TAB_ID);
    // The captain tiles (agent tiles in the reserved tab) never surface as rows.
    expect(container.textContent).not.toContain(CAPTAINS_TAB_NAME);
  });

  it("does not render captain tiles as bottom terminal rows", () => {
    const { container } = render(<WorkspacesList />);
    // The work tile IS present; the captain tiles are NOT.
    expect(container.querySelector('[data-th-ws-row="t1"]')).toBeTruthy();
    const captainsRow = container.querySelector(
      `[data-th-ws-row="${CAPTAINS_TAB_ID}"]`,
    );
    expect(captainsRow).toBeNull();
  });

  it("keeps the exclusion after an adoptRegistry round-trip", () => {
    // A server sync echoes back the tabs (the tab reporter up-syncs the reserved
    // tab too); adoptRegistry re-appends exactly one reserved Captains tab. The
    // bottom list must still exclude it.
    const regTabs: TabReport[] = [
      { id: "t1", name: "Workspace 1", tileIds: ["work0001"] },
      {
        id: CAPTAINS_TAB_ID,
        name: CAPTAINS_TAB_NAME,
        tileIds: ["cap00001", "cap00002"],
      },
    ];
    useWorkspace.getState().adoptRegistry(regTabs);
    // The store still holds exactly one reserved Captains tab with its agent tiles.
    const reserved = useWorkspace
      .getState()
      .tabs.filter((t) => t.id === CAPTAINS_TAB_ID);
    expect(reserved).toHaveLength(1);
    expect(reserved[0].order).toEqual(["cap00001", "cap00002"]);

    const { container } = render(<WorkspacesList />);
    const rows = wsRows(container);
    expect(rows).toEqual(["t1"]);
    expect(container.textContent).not.toContain(CAPTAINS_TAB_NAME);
  });

  it("an ordinary work workspace still closes normally", () => {
    // Two work tabs so the last-tab guard does not block the close.
    useWorkspace.setState({
      tabs: [
        { id: "t1", name: "Workspace 1", order: ["work0001"] },
        { id: "t2", name: "Workspace 2", order: ["work0002"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: ["cap00001"] },
      ],
      activeTabId: "t1",
      focusedId: "work0001",
      terminals: {
        work0001: term("work0001"),
        work0002: term("work0002"),
        cap00001: term("cap00001"),
      },
      poppedOutTabs: [],
    });
    const { container } = render(<WorkspacesList />);
    const row = container.querySelector<HTMLElement>('[data-th-ws-row="t2"]')!;
    const closeBtn = within(row).getByLabelText("Close workspace Workspace 2");
    closeBtn.click();
    const ids = useWorkspace.getState().tabs.map((t) => t.id);
    expect(ids).not.toContain("t2");
    // The reserved Captains tab and the other work tab survive.
    expect(ids).toContain("t1");
    expect(ids).toContain(CAPTAINS_TAB_ID);
  });
});
