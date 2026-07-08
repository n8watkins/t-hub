// Per-tile context-window binding: it must be STRICTLY per owning session
// (`th_<id>`), never by shared cwd. The regression these guard (glitch-header):
// two sessions in the SAME directory (a captain + a crew both in the main
// worktree) once shared one `byCwd` bucket, so one session's context reading
// surfaced on the OTHER's tile. The binding now drops any reading that can't be
// attributed to a tmux session, so a tile only ever shows its own number.
import { describe, it, expect, beforeEach } from "vitest";
import type { StatusSnapshotWire } from "../ipc/client05";
import {
  useSessionContext,
  readContextPct,
  sessionNameForTerminal,
} from "./sessionContext";

/** Build a status snapshot wire frame with sensible defaults. */
function snap(over: Partial<StatusSnapshotWire>): StatusSnapshotWire {
  return {
    sessionId: "sess",
    rateLimitsPresent: false,
    ingestedAtMs: 1000,
    ...over,
  } as StatusSnapshotWire;
}

const state = () => useSessionContext.getState();

describe("sessionContext — strictly per-session context binding", () => {
  beforeEach(() => {
    useSessionContext.setState({ bySession: {} });
  });

  it("files a reading under the owning tmux session and reads it back", () => {
    state().ingest(
      snap({ sessionId: "A", tmuxSession: "th_aaa", cwd: "/repo", contextUsedPct: 30 }),
    );
    expect(readContextPct(state(), "aaa")).toBe(30);
    // The lookup key is derived from the terminal id, not the cwd.
    expect(sessionNameForTerminal("aaa")).toBe("th_aaa");
  });

  it("does NOT leak one session's reading onto another tile in the same cwd", () => {
    // Session A reports from the shared main worktree.
    state().ingest(
      snap({ sessionId: "A", tmuxSession: "th_aaa", cwd: "/main", contextUsedPct: 30 }),
    );
    // Tile B lives in the SAME directory but has reported nothing of its own.
    // Before the fix it would have read A's 30% via the shared cwd bucket.
    expect(readContextPct(state(), "bbb")).toBeNull();
    // A still reads its own value.
    expect(readContextPct(state(), "aaa")).toBe(30);
    // Once B reports, each tile shows its OWN number — no cross-talk.
    state().ingest(
      snap({ sessionId: "B", tmuxSession: "th_bbb", cwd: "/main", contextUsedPct: 72 }),
    );
    expect(readContextPct(state(), "aaa")).toBe(30);
    expect(readContextPct(state(), "bbb")).toBe(72);
  });

  it("drops a reading with no owning tmux session rather than guessing by cwd", () => {
    state().ingest(snap({ sessionId: "X", cwd: "/main", contextUsedPct: 50 }));
    expect(state().bySession).toEqual({});
    // A tile in that cwd shows nothing rather than an unattributed number.
    expect(readContextPct(state(), "somewhere")).toBeNull();
  });

  it("keeps the freshest reading when snapshots arrive out of order", () => {
    state().ingest(
      snap({ tmuxSession: "th_aaa", contextUsedPct: 40, ingestedAtMs: 2000 }),
    );
    // An older snapshot must not clobber the newer one.
    state().ingest(
      snap({ tmuxSession: "th_aaa", contextUsedPct: 10, ingestedAtMs: 1000 }),
    );
    expect(readContextPct(state(), "aaa")).toBe(40);
  });

  it("forget() drops only the given tile's reading", () => {
    state().ingest(snap({ tmuxSession: "th_aaa", contextUsedPct: 30 }));
    state().ingest(snap({ tmuxSession: "th_bbb", contextUsedPct: 60 }));
    state().forget("aaa");
    expect(readContextPct(state(), "aaa")).toBeNull();
    expect(readContextPct(state(), "bbb")).toBe(60);
  });
});
