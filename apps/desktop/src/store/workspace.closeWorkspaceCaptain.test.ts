// adopt-harden F2 (protective default): closeWorkspace must NEVER SIGKILL a
// registered-captain tile that happens to sit in a work tab - it RE-PLACES it into
// the reserved Captains tab instead. This exact vector (a workspace close reaping a
// captain tile mis-placed in a work tab) killed a live captain during a re-org.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

const killTerminal = vi.fn((_id?: string) => Promise.resolve());

vi.mock("../ipc/client", () => ({
  killTerminal: (id: string) => killTerminal(id),
  spawnTerminal: () => Promise.resolve(),
  closeTerminal: () => Promise.resolve(),
}));
// closeWorkspace also drops the Recent cache; stub it web-safe so the fire-and-
// forget import never reaches Tauri.
vi.mock("../ipc/recent", () => ({
  invalidateRecentCache: () => Promise.resolve(),
}));
vi.mock("../ipc/history", () => ({
  invalidateHistoryCache: () => Promise.resolve(),
}));

import {
  useWorkspace,
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  registerCaptainRegistry,
} from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, cwd: "/tmp", title: id, state: "live" };
}

const tab = (id: string) => useWorkspace.getState().tabs.find((t) => t.id === id);

/** closeWorkspace fires killTerminal via a fire-and-forget dynamic import(), so
 *  the calls land on a later microtask - flush before asserting on them. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  killTerminal.mockReset();
  killTerminal.mockResolvedValue(undefined);
  registerCaptainRegistry(() => []);
});

afterEach(() => {
  registerCaptainRegistry(() => []);
});

describe("closeWorkspace protects a registered captain from the reap", () => {
  it("re-places a captain tile into Captains instead of killing it, and kills the work tiles", async () => {
    // "cap" is a registered captain that ended up in a WORK tab (the mis-placement
    // vector); "b" is a genuine work session alongside it. A second work tab exists
    // so the close is permitted by the last-work-tab guard.
    registerCaptainRegistry(() => ["cap"]);
    useWorkspace.setState({
      tabs: [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: "t2", name: "Workspace 2", order: ["cap", "b"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] },
      ],
      activeTabId: "t1",
      focusedId: "a",
      terminals: {
        a: term("a"),
        cap: term("cap"),
        b: term("b"),
      },
      poppedOutTabs: [],
    });

    useWorkspace.getState().closeWorkspace("t2");
    await flush();

    // The work tab is gone...
    expect(tab("t2")).toBeUndefined();
    // ...the captain was RE-PLACED into the reserved Captains tab (survives)...
    expect(tab(CAPTAINS_TAB_ID)?.order).toContain("cap");
    expect(useWorkspace.getState().terminals["cap"]).toBeDefined();
    // ...and it was NOT SIGKILLed, while the genuine work session WAS.
    expect(killTerminal).toHaveBeenCalledWith("b");
    expect(killTerminal).not.toHaveBeenCalledWith("cap");
  });

  it("kills every tile when none are registered captains (unchanged behavior)", async () => {
    registerCaptainRegistry(() => []);
    useWorkspace.setState({
      tabs: [
        { id: "t1", name: "Workspace 1", order: ["a"] },
        { id: "t2", name: "Workspace 2", order: ["b", "c"] },
        { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] },
      ],
      activeTabId: "t1",
      focusedId: "a",
      terminals: { a: term("a"), b: term("b"), c: term("c") },
      poppedOutTabs: [],
    });

    useWorkspace.getState().closeWorkspace("t2");
    await flush();

    expect(tab("t2")).toBeUndefined();
    expect(killTerminal).toHaveBeenCalledWith("b");
    expect(killTerminal).toHaveBeenCalledWith("c");
    expect(tab(CAPTAINS_TAB_ID)?.order).toEqual([]);
  });
});
