// The ADOPT-ONLY default-orchestrator resolution (no spawn, per the audit).
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  isOrchestratorCwd,
  resolveOrchestrator,
  ORCHESTRATOR_CWD_SUFFIX,
} from "./ensureOrchestrator";

// Mocks for the one-click commission glue (the backend command is unit-tested in
// control.rs; here we verify the FRONTEND wiring: fire the command, then designate +
// focus the returned tile, with a double-click in-flight guard).
const controlRequest = vi.fn();
const setOrchestratorId = vi.fn();
const setActiveTab = vi.fn();
const ensureCaptainsTab = vi.fn(() => "captains-tab");
const setFocus = vi.fn();
vi.mock("../ipc/controlClient", () => ({
  controlRequest: (...a: unknown[]) => controlRequest(...a),
}));
vi.mock("../store/captain", () => ({
  useCaptain: {
    getState: () => ({ orchestratorId: "orch-live", setOrchestratorId }),
  },
}));
vi.mock("../store/workspace", () => ({
  useWorkspace: {
    getState: () => ({ setActiveTab, ensureCaptainsTab, setFocus }),
  },
}));

describe("isOrchestratorCwd", () => {
  it("matches the orchestrator home under any HOME (WSL / Windows separators)", () => {
    expect(isOrchestratorCwd("/home/natkins/.t-hub/orchestrator")).toBe(true);
    expect(isOrchestratorCwd("/home/natkins/.t-hub/orchestrator/")).toBe(true);
    expect(isOrchestratorCwd("C:\\Users\\x\\.t-hub\\orchestrator")).toBe(true);
    expect(isOrchestratorCwd(ORCHESTRATOR_CWD_SUFFIX)).toBe(true);
  });

  it("does not match other dirs, empty, or a partial", () => {
    expect(isOrchestratorCwd("/home/x/.t-hub")).toBe(false);
    expect(isOrchestratorCwd("/home/x/.t-hub/orchestrator-other")).toBe(false);
    expect(isOrchestratorCwd("/home/x/project")).toBe(false);
    expect(isOrchestratorCwd(undefined)).toBe(false);
    expect(isOrchestratorCwd("")).toBe(false);
  });
});

describe("resolveOrchestrator (adopt-only, never spawns)", () => {
  const home = "/home/x/.t-hub/orchestrator";

  it("keeps the persisted orchestrator when it is still a live terminal", () => {
    const terminals = { orch1: { cwd: home, state: "live" }, other: { cwd: "/p" } };
    expect(resolveOrchestrator("orch1", terminals)).toBe("orch1");
  });

  it("adopts a live session at the orchestrator home when none is designated", () => {
    const terminals = { a: { cwd: "/p/a" }, b: { cwd: home, state: "live" } };
    expect(resolveOrchestrator(null, terminals)).toBe("b");
  });

  it("adopts by cwd when the persisted id is DEAD (not in the live set)", () => {
    // orch-old is persisted but gone (relaunch); a live one sits at the home.
    const terminals = { fresh: { cwd: home } };
    expect(resolveOrchestrator("orch-old", terminals)).toBe("fresh");
  });

  it("returns null (NO spawn) when there is no orchestrator session at all", () => {
    const terminals = { a: { cwd: "/p/a" }, b: { cwd: "/p/b" } };
    expect(resolveOrchestrator(null, terminals)).toBeNull();
    expect(resolveOrchestrator("dead-id", terminals)).toBeNull();
  });

  it("is idempotent - a second call with the same inputs designates the same id", () => {
    const terminals = { b: { cwd: home } };
    const first = resolveOrchestrator(null, terminals);
    expect(resolveOrchestrator(first, terminals)).toBe(first);
  });
});

describe("commissionOrchestrator (one-click create/adopt/focus)", () => {
  beforeEach(() => {
    controlRequest.mockReset();
    setOrchestratorId.mockReset();
    setActiveTab.mockReset();
    setFocus.mockReset();
  });

  it("fires the backend command, then designates + focuses the returned tile", async () => {
    controlRequest.mockResolvedValue({ terminalId: "new-orch" });
    const { commissionOrchestrator } = await import("./ensureOrchestrator");
    const id = await commissionOrchestrator();
    expect(id).toBe("new-orch");
    expect(controlRequest).toHaveBeenCalledWith("commission_orchestrator", {});
    expect(setOrchestratorId).toHaveBeenCalledWith("new-orch");
    expect(setActiveTab).toHaveBeenCalledWith("captains-tab");
    expect(setFocus).toHaveBeenCalledWith("new-orch");
  });

  it("passes forceRespawn through for the restart stale-token repair", async () => {
    controlRequest.mockResolvedValue({ terminalId: "restored" });
    const { commissionOrchestrator } = await import("./ensureOrchestrator");
    await commissionOrchestrator({ forceRespawn: true });
    expect(controlRequest).toHaveBeenCalledWith("commission_orchestrator", {
      forceRespawn: true,
    });
  });

  it("dedups a concurrent double-click (second in-flight call is a no-op)", async () => {
    let resolveFirst: (v: { terminalId: string }) => void = () => {};
    controlRequest.mockReturnValueOnce(
      new Promise((r) => {
        resolveFirst = r;
      }),
    );
    const { commissionOrchestrator } = await import("./ensureOrchestrator");
    const p1 = commissionOrchestrator();
    const p2 = commissionOrchestrator(); // in-flight -> immediate null, no 2nd call
    expect(await p2).toBeNull();
    // p1's controlRequest fires only after its dynamic import() resolves; flush the
    // microtask/timer queue so the assertion sees exactly the one in-flight call.
    await vi.waitFor(() => expect(controlRequest).toHaveBeenCalledTimes(1));
    resolveFirst({ terminalId: "new-orch" });
    expect(await p1).toBe("new-orch");
  });

  it("returns null and does not designate when the backend yields no tile", async () => {
    controlRequest.mockResolvedValue({});
    const { commissionOrchestrator } = await import("./ensureOrchestrator");
    expect(await commissionOrchestrator()).toBeNull();
    expect(setOrchestratorId).not.toHaveBeenCalled();
  });
});
