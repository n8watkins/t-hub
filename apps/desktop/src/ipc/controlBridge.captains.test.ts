// Captain-chat phase 2: the control bridge's captains reconciliation layer - the
// most race-prone code in the feature (a boot fetch racing live sync_captains
// forwards). These pin the guarantees: an older snapshot is rejected, an equal
// seq is idempotent, an empty snapshot never wipes local pins (A1), and the boot
// bootstrap suppresses mid-loop partial forwards then adopts only the final
// snapshot.
import { beforeEach, describe, expect, it, vi } from "vitest";

// Drive the bootstrap deterministically by mocking the control transport. A
// per-command handler (value or function) lets list_captains return different
// snapshots on its two bootstrap calls (initial vs post-claim).
const { controlRequests, mockState } = vi.hoisted(() => ({
  controlRequests: [] as Array<{ command: string; args: unknown }>,
  mockState: { handlers: {} as Record<string, unknown> },
}));

vi.mock("./controlClient", () => ({
  controlRequest: (command: string, args: unknown) => {
    controlRequests.push({ command, args });
    const h = mockState.handlers[command];
    return Promise.resolve(typeof h === "function" ? h(args) : (h ?? {}));
  },
  onControlEvent: () => () => {},
}));

import {
  applyControl,
  adoptCaptainsSnapshot,
  bootstrapCaptains,
  __resetCaptainsReconcileForTest,
  __setCaptainsBootstrappingForTest,
} from "./controlBridge";
import { useCaptain } from "../store/captain";
import { useWorkspace } from "../store/workspace";

// A raw wire captain (as `sync_captains` sends it). Typed loosely because the
// adapter takes `unknown` and must tolerate BOTH schema versions; the helper emits
// the modern shape (terminalId + CrewRef crew), and a dedicated test below feeds the
// LEGACY shape to prove the mixed-window back-compat.
function claim(
  id: string,
  workspaceTabIds: string[] = [],
  crew: string[] = [],
): Record<string, unknown> {
  return {
    terminalId: id,
    shipSlug: `ship-${id}`,
    workspaceTabIds,
    crew: crew.map((c) => ({ terminalId: c })),
  };
}

function seedCaptains(ids: string[]): void {
  useCaptain.setState({
    captainIds: ids,
    claims: {},
    activeCaptainId: ids[0] ?? null,
    open: false,
    anchorMenuOpen: false,
  });
}

beforeEach(() => {
  localStorage.clear();
  controlRequests.length = 0;
  mockState.handlers = {};
  __resetCaptainsReconcileForTest();
  // A workspace whose active tab holds the captain tiles, so the bootstrap's
  // live-tile filter can see the pins it should claim.
  useWorkspace.setState({
    tabs: [{ id: "t1", name: "W1", order: ["capA", "capB", "capC"] }],
    activeTabId: "t1",
    focusedId: "capA",
    terminals: {},
    poppedOutTabs: [],
  });
  seedCaptains([]);
});

describe("adoptCaptainsSnapshot seq guard", () => {
  it("rejects a stale (older-seq) snapshot outright", () => {
    expect(adoptCaptainsSnapshot({ seq: 5, captains: [claim("capA", ["t1"])] })).toBe(
      true,
    );
    expect(useCaptain.getState().captainIds).toEqual(["capA"]);
    // A lower seq is a stale forward the window already superseded: rejected,
    // no store change.
    expect(
      adoptCaptainsSnapshot({ seq: 3, captains: [claim("capA"), claim("capB")] }),
    ).toBe(false);
    expect(useCaptain.getState().captainIds).toEqual(["capA"]);
  });

  it("accepts an equal-seq snapshot idempotently", () => {
    adoptCaptainsSnapshot({ seq: 5, captains: [claim("capA", ["t1"])] });
    const before = useCaptain.getState().captainIds;
    // Equal seq is NOT older, so it is accepted; re-applying the same data is a
    // no-op on membership.
    expect(adoptCaptainsSnapshot({ seq: 5, captains: [claim("capA", ["t1"])] })).toBe(
      true,
    );
    expect(useCaptain.getState().captainIds).toEqual(before);
    expect(useCaptain.getState().claims["capA"].workspaceTabIds).toEqual(["t1"]);
  });

  it("adopts the LEGACY v0 wire shape (captainSessionId + string crew) - mixed window", () => {
    // Item-2 re-key back-compat: a pre-item-2 server (or an on-disk v0 record read
    // through) sends `captainSessionId` + `crew: [string]`. The adapter must still
    // adopt it - keyed by terminal, crew upgraded to CrewRef - so a mixed
    // client/server window never drops a live pin. A bypass (requiring the new
    // field) would silently lose the captain here.
    expect(
      adoptCaptainsSnapshot({
        seq: 9,
        captains: [
          { captainSessionId: "capLegacy", shipSlug: "old", workspaceTabIds: ["t1"], crew: ["c1"] },
        ],
      }),
    ).toBe(true);
    expect(useCaptain.getState().captainIds).toEqual(["capLegacy"]);
    expect(useCaptain.getState().claims["capLegacy"].crew).toEqual([{ terminalId: "c1" }]);
  });

  it("ignores a malformed snapshot (missing seq / non-array captains)", () => {
    seedCaptains(["capA"]);
    expect(adoptCaptainsSnapshot(null)).toBe(false);
    expect(adoptCaptainsSnapshot({ captains: [] })).toBe(false);
    expect(adoptCaptainsSnapshot({ seq: 1, captains: "nope" })).toBe(false);
    expect(useCaptain.getState().captainIds).toEqual(["capA"]);
  });
});

describe("adoptCaptainsSnapshot A1 empty guard", () => {
  it("does not let an empty boot snapshot wipe local pins", () => {
    // At boot lastCaptainsSeq is -1, so a zero-captain snapshot (a registry load
    // failure / reconnect-before-load) passes the seq guard - the A1 guard in
    // the captain store is what keeps the migrated pins.
    seedCaptains(["capA", "capB"]);
    expect(adoptCaptainsSnapshot({ seq: 0, captains: [] })).toBe(true);
    expect(useCaptain.getState().captainIds).toEqual(["capA", "capB"]);
  });
});

describe("cortana role survives the wire -> record conversion (crown sync)", () => {
  // The captains-view crown must reflect the registry's Cortana holder live. A
  // claim_captain role:cortana on ANOTHER client (or restored at this window's
  // reload) rides a sync_captains record carrying role:"cortana"; adoptCaptains-
  // Snapshot must PRESERVE that role so the store's reconcile can adopt the crown.
  // Bypass-would-fail: drop `role: ...` from the record it pushes and this fails
  // (the crown never syncs cross-client / on reload).
  it("adopts a wire cortana record into orchestratorId", () => {
    useWorkspace.setState({
      tabs: [{ id: "t1", name: "W1", order: ["capA"] }],
      activeTabId: "t1",
      focusedId: "capA",
      terminals: { capA: { id: "capA", tmuxSession: "th_capA", cwd: "/tmp", title: "capA", state: "live" } },
      poppedOutTabs: [],
    });
    useCaptain.setState({ orchestratorId: null });
    const ok = adoptCaptainsSnapshot({
      seq: 1,
      captains: [{ terminalId: "capA", shipSlug: "cortana", role: "cortana", workspaceTabIds: [], crew: [] }],
    });
    expect(ok).toBe(true);
    expect(useCaptain.getState().orchestratorId).toBe("capA");
    // Role split: a cortana record is the crown, never a summonable captain pin.
    expect(useCaptain.getState().captainIds).toEqual([]);
  });
});

describe("sync_captains forward suppression during bootstrap", () => {
  it("suppresses a mid-loop sync_captains forward, then resumes after bootstrap", () => {
    seedCaptains(["capA"]);
    __setCaptainsBootstrappingForTest(true);
    // A partial forward mid-bootstrap (only some pins claimed so far) would drop
    // the not-yet-claimed pins - it must be ignored.
    applyControl("sync_captains", { sync: { seq: 1, captains: [] } });
    expect(useCaptain.getState().captainIds).toEqual(["capA"]);

    __setCaptainsBootstrappingForTest(false);
    // Bootstrap over: a real forward now applies normally.
    applyControl("sync_captains", {
      sync: { seq: 2, captains: [claim("capA"), claim("capB", ["t1"])] },
    });
    expect(useCaptain.getState().captainIds).toEqual(["capA", "capB"]);
  });
});

describe("bootstrapCaptains", () => {
  it("claims live-tile pins the server lacks, then adopts the final snapshot", async () => {
    // Local has capA (server-unknown, live tile) and capB (already claimed).
    seedCaptains(["capA", "capB"]);
    let listCall = 0;
    mockState.handlers = {
      list_captains: () => {
        listCall += 1;
        return listCall === 1
          ? { seq: 5, captains: [claim("capB", ["t1"])] } // capA missing
          : { seq: 6, captains: [claim("capB", ["t1"]), claim("capA", ["t1"])] };
      },
      claim_captain: {},
    };

    await bootstrapCaptains();

    // capA (missing + live tile) was claimed; capB (already on the server) was not.
    expect(
      controlRequests.filter((r) => r.command === "claim_captain"),
    ).toEqual([{ command: "claim_captain", args: { captainSessionId: "capA" } }]);
    // The FINAL snapshot (both captains) was adopted.
    const s = useCaptain.getState();
    expect([...s.captainIds].sort()).toEqual(["capA", "capB"]);
    expect(s.claims["capA"].workspaceTabIds).toEqual(["t1"]);
  });

  it("does not claim a pin whose tile is gone (no garbage server claim)", async () => {
    // capZ is pinned locally but has NO live tile (not in any tab order).
    seedCaptains(["capZ"]);
    mockState.handlers = { list_captains: { seq: 2, captains: [] } };

    await bootstrapCaptains();

    expect(
      controlRequests.some((r) => r.command === "claim_captain"),
    ).toBe(false);
  });
});
