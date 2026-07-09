// restartTerminal: recover a frozen session by spawning a FRESH tmux session in
// the same cwd, swapping it into the OLD tile's exact tab + slot, then killing
// the old session. This guards the placement contract (same tab, same index) and
// that the old session is killed and its live map entry dropped.
import { describe, it, expect, beforeEach, vi } from "vitest";

const spawnTerminal = vi.fn();
const killTerminal = vi.fn((_id?: string) => Promise.resolve());

vi.mock("../ipc/client", () => ({
  spawnTerminal: (opts: unknown) => spawnTerminal(opts),
  killTerminal: (id: string) => killTerminal(id),
  closeTerminal: () => Promise.resolve(),
}));

import { useWorkspace } from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/repo"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, title: id, cwd, state: "live" };
}

beforeEach(() => {
  spawnTerminal.mockReset();
  killTerminal.mockReset();
  killTerminal.mockResolvedValue(undefined);
  // Two tabs; the target tile sits at index 1 of tab t1, between two siblings.
  useWorkspace.setState({
    tabs: [
      { id: "t1", name: "One", order: ["a", "old1", "b"] },
      { id: "t2", name: "Two", order: ["c"] },
    ],
    terminals: {
      a: term("a"),
      old1: term("old1", "/repo/frozen"),
      b: term("b"),
      c: term("c"),
    },
    activeTabId: "t1",
    focusedId: "old1",
    userLabels: {},
    labels: {},
  });
});

describe("restartTerminal", () => {
  it("spawns a fresh session in the same cwd and drops it in the same tab slot", async () => {
    spawnTerminal.mockResolvedValue(term("new1", "/repo/frozen"));
    const newId = await useWorkspace.getState().restartTerminal("old1");

    expect(newId).toBe("new1");
    // Spawned rooted at the frozen tile's cwd.
    expect(spawnTerminal).toHaveBeenCalledWith({ cwd: "/repo/frozen" });
    // Same tab, SAME slot (index 1), old id gone.
    const t1 = useWorkspace.getState().tabs.find((t) => t.id === "t1");
    expect(t1?.order).toEqual(["a", "new1", "b"]);
    // The other tab is untouched.
    expect(useWorkspace.getState().tabs.find((t) => t.id === "t2")?.order).toEqual([
      "c",
    ]);
    // Live map: fresh in, old out. Focus followed the restart.
    const { terminals, focusedId } = useWorkspace.getState();
    expect(terminals.new1).toBeTruthy();
    expect(terminals.old1).toBeUndefined();
    expect(focusedId).toBe("new1");
    // The OLD tmux session is killed (process tree) after the swap.
    expect(killTerminal).toHaveBeenCalledWith("old1");
  });

  it("leaves the tile untouched and does not kill when the spawn fails", async () => {
    spawnTerminal.mockRejectedValue(new Error("spawn boom"));
    const newId = await useWorkspace.getState().restartTerminal("old1");

    expect(newId).toBeNull();
    // Old tile still exactly where it was; nothing killed.
    const t1 = useWorkspace.getState().tabs.find((t) => t.id === "t1");
    expect(t1?.order).toEqual(["a", "old1", "b"]);
    expect(useWorkspace.getState().terminals.old1).toBeTruthy();
    expect(killTerminal).not.toHaveBeenCalled();
  });

  it("no-ops for an unknown terminal id", async () => {
    const newId = await useWorkspace.getState().restartTerminal("ghost");
    expect(newId).toBeNull();
    expect(spawnTerminal).not.toHaveBeenCalled();
  });
});
