// Captain notification attribution: the resolver that names WHICH captain a
// notification came from. A captain session yields "Captain <name>" (its stable
// identity, never the volatile Claude title), the orchestrator yields its brand
// name, and a non-captain / unresolvable session yields null (generic wording).
import { describe, it, expect, beforeEach } from "vitest";
import {
  captainAttributionForSession,
  captainSubjectForSession,
  terminalIdForSession,
} from "./captainAttribution";
import { useSupervision } from "../store/supervision";
import { useWorkspace } from "../store/workspace";
import { useCaptain } from "../store/captain";
import { ORCHESTRATOR_DISPLAY_NAME } from "./ensureOrchestrator";

/** Seed the supervision reverse index so `sess` maps to tile `id` (`th_<id>`). */
function bindSession(id: string, sess: string): void {
  useSupervision.setState({
    trees: {},
    statuses: {},
    snapshots: {},
    sessionIdByTmux: { [`th_${id}`]: sess },
  });
}

beforeEach(() => {
  useSupervision.setState({
    trees: {},
    statuses: {},
    snapshots: {},
    sessionIdByTmux: {},
  });
  useWorkspace.setState({ tabs: [], terminals: {}, userLabels: {}, labels: {} });
  useCaptain.setState({ captainIds: [], claims: {}, orchestratorId: null });
});

describe("captainAttribution", () => {
  it("resolves a tile id from a session id via the supervision index", () => {
    bindSession("cap00001", "sess-1");
    expect(terminalIdForSession("sess-1")).toBe("cap00001");
    expect(terminalIdForSession("nope")).toBeNull();
  });

  it("names a pinned captain by its stable rename", () => {
    bindSession("cap00001", "sess-1");
    useWorkspace.setState({
      tabs: [],
      terminals: { cap00001: { id: "cap00001", tmuxSession: "th_cap00001", title: "t", cwd: "/tmp/ship", state: "live" } },
      userLabels: { cap00001: "alpha" },
      labels: { cap00001: "alpha" },
    });
    useCaptain.setState({ captainIds: ["cap00001"], claims: {}, orchestratorId: null });
    const a = captainAttributionForSession("sess-1");
    expect(a).toEqual({ isOrchestrator: false, name: "alpha" });
    expect(captainSubjectForSession("sess-1")).toBe("Captain alpha");
  });

  it("falls back to the cwd folder, then the registry ship slug", () => {
    bindSession("cap00002", "sess-2");
    // No rename, no cwd → the ship slug from the captains registry is used.
    useWorkspace.setState({
      tabs: [],
      terminals: { cap00002: { id: "cap00002", tmuxSession: "th_cap00002", title: "t", cwd: "", state: "live" } },
      userLabels: {},
      labels: {},
    });
    useCaptain.setState({
      captainIds: ["cap00002"],
      claims: {
        cap00002: {
          shipSlug: "bravo-ship",
          captainSessionId: "cap00002",
          workspaceTabIds: [],
          crew: [],
        },
      },
      orchestratorId: null,
    });
    expect(captainSubjectForSession("sess-2")).toBe("Captain bravo-ship");

    // A cwd folder beats the slug when present.
    useWorkspace.setState({
      tabs: [],
      terminals: { cap00002: { id: "cap00002", tmuxSession: "th_cap00002", title: "t", cwd: "/work/charlie", state: "live" } },
      userLabels: {},
      labels: {},
    });
    expect(captainSubjectForSession("sess-2")).toBe("Captain charlie");
  });

  it("names the orchestrator by its brand, with no 'Captain' prefix", () => {
    bindSession("orch0001", "sess-orch");
    useCaptain.setState({
      captainIds: [],
      claims: {},
      orchestratorId: "orch0001",
    });
    const a = captainAttributionForSession("sess-orch");
    expect(a).toEqual({ isOrchestrator: true, name: ORCHESTRATOR_DISPLAY_NAME });
    expect(captainSubjectForSession("sess-orch")).toBe(ORCHESTRATOR_DISPLAY_NAME);
  });

  it("returns null for a non-captain session (generic wording stands)", () => {
    bindSession("crew0001", "sess-crew");
    useWorkspace.setState({
      tabs: [],
      terminals: { crew0001: { id: "crew0001", tmuxSession: "th_crew0001", title: "t", cwd: "/tmp/x", state: "live" } },
      userLabels: { crew0001: "some crew" },
      labels: {},
    });
    useCaptain.setState({ captainIds: [], claims: {}, orchestratorId: null });
    expect(captainAttributionForSession("sess-crew")).toBeNull();
    expect(captainSubjectForSession("sess-crew")).toBeNull();
  });

  it("returns null when the session has no resolvable tile", () => {
    expect(captainSubjectForSession("ghost")).toBeNull();
  });
});
