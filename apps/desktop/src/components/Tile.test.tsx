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
// Capture the header menu's click-to-copy without a Tauri clipboard host.
const clipboardWrites: string[] = [];
vi.mock("../lib/clipboard", () => ({
  clipboardWrite: (text: string) => {
    clipboardWrites.push(text);
    return Promise.resolve();
  },
  clipboardRead: () => Promise.resolve(""),
}));
// The Cortana mark is a fire-and-forget control mutation; swallow it in tests.
vi.mock("../ipc/controlClient", () => ({
  controlRequest: () => Promise.resolve({}),
  onControlEvent: () => () => {},
}));

import { Tile } from "./Tile";
import { useCaptain } from "../store/captain";
import { usePanels } from "../store/panels";
import { useSupervision } from "../store/supervision";
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
    claudeTitles: {},
  });
  useCaptain.setState({ orchestratorId: null, captainIds: [], claims: {} });
  useSupervision.setState({ sessionIdByTmux: {} });
  usePanels.setState({ tab: {}, devUrl: {}, previewUrl: {} });
  clipboardWrites.length = 0;
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

describe("Tile Run and Preview entry point", () => {
  it("offers one Run + Preview tab and no separate Dev tab", () => {
    const header = renderTile("cap00001");

    expect(within(header).getByTitle("Run + Preview view")).toBeTruthy();
    expect(within(header).queryByTitle("Preview view")).toBeNull();
    expect(within(header).queryByTitle("Dev view")).toBeNull();
  });
});

describe("Tile responsive header controls", () => {
  it("keeps stable tab names while rendering icon, full, and short variants", () => {
    const header = renderTile("cap00001");
    const tabs = [
      { label: "Terminal", short: "Term", pressed: "true" },
      { label: "Files", short: "Files", pressed: "false" },
      { label: "Run + Preview", short: "Run", pressed: "false" },
    ];

    for (const tab of tabs) {
      const button = within(header).getByRole("button", {
        name: tab.label,
      });
      expect(button.getAttribute("title")).toBe(`${tab.label} view`);
      expect(button.getAttribute("aria-pressed")).toBe(tab.pressed);
      expect(button.classList.contains("h-6")).toBe(true);
      expect(button.querySelector(".th-tab-icon")?.getAttribute("aria-hidden")).toBe(
        "true",
      );
      expect(button.querySelector(".th-tab-label")?.textContent).toBe(tab.label);
      const short = button.querySelector(".th-tab-label-short");
      expect(short?.textContent).toBe(tab.short);
      expect(short?.getAttribute("aria-hidden")).toBe("true");
    }

    expect(within(header).queryByRole("button", { name: "Terminal view" })).toBeNull();
    expect(within(header).queryByRole("button", { name: "Board" })).toBeNull();
    expect(header.querySelector(".th-tab-list")).toBeTruthy();
  });

  it("switches views without changing the tab names", () => {
    const header = renderTile("cap00001");
    const files = within(header).getByRole("button", { name: "Files" });

    act(() => {
      fireEvent.click(files);
    });

    expect(usePanels.getState().tab.cap00001).toBe("files");
    expect(files.getAttribute("aria-pressed")).toBe("true");
    expect(within(header).getByRole("button", { name: "Terminal" })).toBeTruthy();
  });

  it("gives every persistent header action a 24px target", () => {
    const header = renderTile("cap00001");
    const actions = [
      "Refresh terminal",
      "Kill and restart session",
      "Terminal colors",
      "Fullscreen tile",
      "Kill session",
    ];

    for (const name of actions) {
      const button = within(header).getByRole("button", { name });
      expect(button.classList.contains("th-header-control")).toBe(true);
      expect(button.classList.contains("h-6")).toBe(true);
      expect(button.classList.contains("w-6")).toBe(true);
    }
  });

  it("marks dual Captain and Cortana identity for minimum-width compaction", () => {
    useWorkspace.setState((state) => ({
      terminals: {
        ...state.terminals,
        cap00001: { ...state.terminals.cap00001, title: "codex" },
      },
    }));
    useCaptain.setState({
      orchestratorId: "cap00001",
      captainIds: ["cap00001"],
    });
    const header = renderTile("cap00001");
    const root = header.closest<HTMLElement>('[data-tile-id="cap00001"]');

    expect(root?.dataset.captain).toBe("1");
    expect(root?.dataset.orchestrator).toBe("1");
    expect(header.querySelector(".th-client-icon")).toBeTruthy();
    expect(header.querySelector(".th-captain-marker")).toBeTruthy();
    expect(header.querySelector(".th-tile-title")?.textContent).toBe("Cortana");
  });

  it("limits size containment to the header and keeps floating surfaces and the body outside", () => {
    const header = renderTile("cap00001");
    const root = header.closest<HTMLElement>('[data-tile-id="cap00001"]');
    const container = header.parentElement;
    const body = root?.lastElementChild;

    expect(root).toBeTruthy();
    expect(container?.classList.contains("th-tile-header-container")).toBe(true);
    expect(container?.parentElement).toBe(root);
    expect(container?.children).toHaveLength(1);
    expect(root?.classList.contains("th-tile-container")).toBe(false);
    expect(body).toBeTruthy();
    expect(body?.parentElement).toBe(root);
    expect(container?.contains(body ?? null)).toBe(false);

    act(() => {
      fireEvent.contextMenu(header);
    });
    const menu = within(document.body)
      .getByText("Terminal ID")
      .closest<HTMLElement>(".fixed.z-50");
    expect(menu).toBeTruthy();
    expect(container?.contains(menu ?? null)).toBe(false);

    act(() => {
      fireEvent.click(within(menu!).getByTitle("Copy Terminal ID"));
    });
    const toast = within(document.body).getByRole("status");
    expect(container?.contains(toast)).toBe(false);

    act(() => {
      fireEvent.contextMenu(header);
    });
    const restartItem = within(document.body).getByText("Kill & restart session");
    act(() => {
      fireEvent.click(restartItem);
    });
    const dialog = within(document.body).getByRole("alertdialog");
    expect(container?.contains(dialog)).toBe(false);

    const colorButton = within(header).getByRole("button", {
      name: "Terminal colors",
    });
    vi.spyOn(colorButton, "getBoundingClientRect").mockReturnValue({
      right: 115,
      bottom: 45,
    } as DOMRect);
    act(() => {
      fireEvent.click(colorButton);
    });
    const colorMenu = within(document.body)
      .getByText("Reset to theme")
      .closest<HTMLElement>(".fixed.z-50");
    expect(colorMenu).toBeTruthy();
    expect(container?.contains(colorMenu ?? null)).toBe(false);
    expect(colorMenu?.style.left).toBe("8px");
  });
});

describe("Tile header context menu: IDs + Mark as Cortana", () => {
  /** Right-click the header to open the context menu, then return the menu's
   *  root (it renders fixed to the document, not inside the header). */
  function openMenu(terminalId: string): HTMLElement {
    const header = renderTile(terminalId);
    act(() => {
      fireEvent.contextMenu(header);
    });
    // The menu is the fixed panel that holds the "Terminal ID" row.
    const item = within(document.body).getByText("Terminal ID");
    const menu = item.closest<HTMLElement>(".fixed.z-50");
    expect(menu).toBeTruthy();
    return menu!;
  }

  it("shows the Terminal ID (copyable) and copies it on click", () => {
    const menu = openMenu("cap00001");
    // The 8-char tmux-derived id is shown verbatim under the label.
    expect(menu.textContent).toContain("cap00001");
    const idRow = within(menu).getByTitle("Copy Terminal ID");
    act(() => {
      fireEvent.click(idRow);
    });
    expect(clipboardWrites).toEqual(["cap00001"]);
    // A one-line ack toast confirms the copy.
    expect(document.body.textContent).toContain("Copied Terminal ID");
  });

  it("hides the Claude Session ID row until a session is bound, then shows + copies it", () => {
    // Unbound: no session id in the supervision reverse index.
    let menu = openMenu("cap00001");
    expect(within(menu).queryByTitle("Copy Claude Session ID")).toBeNull();
    // Identify the foreground client as Claude and bind its UUID to this tile's
    // tmux session (`th_<id>`), as a statusline snapshot would, then re-open.
    act(() => {
      useWorkspace.setState((state) => ({
        terminals: {
          ...state.terminals,
          cap00001: {
            ...state.terminals.cap00001,
            title: "claude",
          },
        },
      }));
      useSupervision.setState({
        sessionIdByTmux: { th_cap00001: "uuid-abc-123" },
      });
    });
    menu = openMenu("cap00001");
    const sessionRow = within(menu).getByTitle("Copy Claude Session ID");
    expect(sessionRow.textContent).toContain("uuid-abc-123");
    act(() => {
      fireEvent.click(sessionRow);
    });
    expect(clipboardWrites).toEqual(["uuid-abc-123"]);
  });

  it("does not label a stale Claude binding as the session ID of a Codex tile", () => {
    act(() => {
      useWorkspace.setState((state) => ({
        terminals: {
          ...state.terminals,
          cap00001: {
            ...state.terminals.cap00001,
            title: "codex",
          },
        },
      }));
      useSupervision.setState({
        sessionIdByTmux: { th_cap00001: "stale-claude-uuid" },
      });
    });

    const menu = openMenu("cap00001");
    expect(within(menu).queryByTitle("Copy Claude Session ID")).toBeNull();
    expect(menu.textContent).not.toContain("stale-claude-uuid");
  });

  it("does not show a stale Claude ID for node-hosted Codex with registry identity", () => {
    act(() => {
      useWorkspace.setState((state) => ({
        terminals: {
          ...state.terminals,
          cap00001: { ...state.terminals.cap00001, title: "node" },
        },
        claudeTitles: { cap00001: "Claude review" },
        labels: { cap00001: "Claude review" },
      }));
      useCaptain.setState({
        claims: {
          cap00001: {
            terminalId: "cap00001",
            shipSlug: "ship-cap00001",
            provider: "codex",
            harness: "codex",
            providerSessionId: "codex-thread",
            workspaceTabIds: ["t1"],
            crew: [],
          },
        },
      });
      useSupervision.setState({
        sessionIdByTmux: { th_cap00001: "stale-claude-uuid" },
      });
    });

    const menu = openMenu("cap00001");
    expect(within(menu).queryByTitle("Copy Claude Session ID")).toBeNull();
    expect(menu.textContent).not.toContain("stale-claude-uuid");
  });

  it("shows a registry-backed Claude ID before supervision rehydrates", () => {
    act(() => {
      useWorkspace.setState((state) => ({
        terminals: {
          ...state.terminals,
          cap00001: { ...state.terminals.cap00001, title: "node" },
        },
      }));
      useCaptain.setState({
        claims: {
          cap00001: {
            terminalId: "cap00001",
            shipSlug: "ship-cap00001",
            provider: "claude",
            harness: "claude",
            providerSessionId: "restored-claude-id",
            workspaceTabIds: ["t1"],
            crew: [],
          },
        },
      });
      useSupervision.setState({ sessionIdByTmux: {} });
    });

    const menu = openMenu("cap00001");
    const sessionRow = within(menu).getByTitle("Copy Claude Session ID");
    expect(sessionRow.textContent).toContain("restored-claude-id");
    act(() => fireEvent.click(sessionRow));
    expect(clipboardWrites).toEqual(["restored-claude-id"]);
  });

  it("labels the mark affordance 'Mark as Cortana' and 'Unmark Cortana' when set", () => {
    let menu = openMenu("cap00001");
    expect(within(menu).getByText("Mark as Cortana")).toBeTruthy();
    // Mark it, then re-open: the affordance flips to the unmark label.
    act(() => {
      useCaptain.setState({ orchestratorId: "cap00001" });
    });
    menu = openMenu("cap00001");
    expect(within(menu).getByText("Unmark Cortana")).toBeTruthy();
  });

  it("keeps kill and restart available when the narrow header hides it", () => {
    const menu = openMenu("cap00001");
    act(() => {
      fireEvent.click(within(menu).getByText("Kill & restart session"));
    });

    const dialog = document.querySelector<HTMLElement>('[role="alertdialog"]');
    expect(dialog).toBeTruthy();
    expect(dialog?.textContent).toContain("Kill & restart this session?");
  });

  it("marking Cortana from the menu sets the designation and flashes the next-steps hint", () => {
    const menu = openMenu("cap00001");
    act(() => {
      fireEvent.click(within(menu).getByText("Mark as Cortana"));
    });
    expect(useCaptain.getState().orchestratorId).toBe("cap00001");
    // The honest hint points at the still-unbuilt follow-on flow.
    expect(document.body.textContent).toContain("Marked as Cortana");
    expect(document.body.textContent).toContain("capability elevation");
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
