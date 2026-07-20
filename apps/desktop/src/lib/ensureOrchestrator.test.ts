import { describe, it, expect } from "vitest";
import {
  CORTANA_RECONCILE_OPERATION_ID,
  isOrchestratorCwd,
  parseCortanaReconcileResult,
  resolveOrchestrator,
  ORCHESTRATOR_CWD_SUFFIX,
} from "./ensureOrchestrator";

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

describe("parseCortanaReconcileResult", () => {
  it("accepts one authoritative healthy identity", () => {
    expect(
      parseCortanaReconcileResult({
        operationId: CORTANA_RECONCILE_OPERATION_ID,
        action: "recover",
        healthy: true,
        terminalId: "c0ffee01",
        identityId: "identity-cortana",
        generation: 2,
        degradedReason: null,
      }),
    ).toMatchObject({ healthy: true, generation: 2 });
  });

  it("preserves an explicit degraded reason", () => {
    expect(
      parseCortanaReconcileResult({
        operationId: CORTANA_RECONCILE_OPERATION_ID,
        action: "degraded",
        healthy: false,
        terminalId: null,
        identityId: "identity-cortana",
        generation: 4,
        degradedReason: "duplicate authoritative generation",
      }).degradedReason,
    ).toBe("duplicate authoritative generation");
  });

  it("rejects false health and malformed evidence", () => {
    expect(() =>
      parseCortanaReconcileResult({
        operationId: CORTANA_RECONCILE_OPERATION_ID,
        action: "keep",
        healthy: true,
        terminalId: null,
        identityId: "identity-cortana",
        generation: 1,
        degradedReason: null,
      }),
    ).toThrow("claimed health");
  });
});
