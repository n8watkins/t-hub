// restartTerminal: recover a frozen session by spawning a FRESH tmux session in
// the same cwd, swapping it into the OLD tile's exact tab + slot, then killing
// the old session. This guards the placement contract (same tab, same index) and
// that the old session is killed and its live map entry dropped.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const spawnTerminal = vi.fn();
const killTerminal = vi.fn((_id?: string) => Promise.resolve());
const notify = vi.fn();
let consoleError: ReturnType<typeof vi.spyOn>;

vi.mock("../ipc/client", () => ({
  spawnTerminal: (opts: unknown) => spawnTerminal(opts),
  killTerminal: (id: string) => killTerminal(id),
  closeTerminal: () => Promise.resolve(),
}));
vi.mock("../lib/notify", () => ({
  notify: (kind: string, title: string, body?: string) =>
    notify(kind, title, body),
}));

import { useWorkspace } from "./workspace";
import type { TerminalInfo } from "../ipc/types";

function term(id: string, cwd = "/repo"): TerminalInfo {
  return { id, tmuxSession: `th_${id}`, title: id, cwd, state: "live" };
}

beforeEach(() => {
  consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
  spawnTerminal.mockReset();
  killTerminal.mockReset();
  notify.mockReset();
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

afterEach(() => {
  consoleError.mockRestore();
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
    expect(consoleError).toHaveBeenCalledWith(
      "restartTerminal failed",
      expect.any(Error),
    );
  });

  it("no-ops for an unknown terminal id", async () => {
    const newId = await useWorkspace.getState().restartTerminal("ghost");
    expect(newId).toBeNull();
    expect(spawnTerminal).not.toHaveBeenCalled();
  });

  it("retries the kill once and surfaces a notice when the old session won't die", async () => {
    spawnTerminal.mockResolvedValue(term("new1", "/repo/frozen"));
    killTerminal.mockRejectedValue(new Error("kill boom"));

    const newId = await useWorkspace.getState().restartTerminal("old1");
    // The swap still stands — a kill failure must not block the recovery.
    expect(newId).toBe("new1");
    expect(useWorkspace.getState().tabs.find((t) => t.id === "t1")?.order).toEqual([
      "a",
      "new1",
      "b",
    ]);
    // Kill attempted TWICE (initial + one retry), then a visible error notice.
    await vi.waitFor(() => expect(killTerminal).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(notify).toHaveBeenCalledTimes(1));
    expect(notify.mock.calls[0][0]).toBe("error");
    expect(notify.mock.calls[0][2]).toContain("old1");
    expect(consoleError).toHaveBeenCalledWith(
      "restartTerminal: kill old session failed after retry",
      expect.any(Error),
    );
  });

  it("recovers on the retry: no notice when the second kill succeeds", async () => {
    spawnTerminal.mockResolvedValue(term("new1", "/repo/frozen"));
    killTerminal
      .mockRejectedValueOnce(new Error("transient"))
      .mockResolvedValueOnce(undefined);

    await useWorkspace.getState().restartTerminal("old1");
    await vi.waitFor(() => expect(killTerminal).toHaveBeenCalledTimes(2));
    // Let any (should-not-exist) notice path settle, then assert it stayed quiet.
    await Promise.resolve();
    expect(notify).not.toHaveBeenCalled();
  });
});
