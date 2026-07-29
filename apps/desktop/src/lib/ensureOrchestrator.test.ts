import { afterEach, describe, it, expect, vi } from "vitest";
import {
  CORTANA_RECONCILE_OPERATION_ID,
  createCortanaReconciliationMonitor,
  isOrchestratorCwd,
  parseCortanaReconcileResult,
  resolveOrchestrator,
  ORCHESTRATOR_CWD_SUFFIX,
} from "./ensureOrchestrator";

afterEach(() => {
  vi.useRealTimers();
});

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

describe("resolveOrchestrator (pure frontend designation)", () => {
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
    expect(() =>
      parseCortanaReconcileResult({
        operationId: CORTANA_RECONCILE_OPERATION_ID,
        action: "keep",
        healthy: true,
        terminalId: "   ",
        identityId: "identity-cortana",
        generation: 1,
        degradedReason: null,
      }),
    ).toThrow("malformed identity");
  });
});

describe("createCortanaReconciliationMonitor", () => {
  const flushPromises = async () => {
    for (let index = 0; index < 6; index += 1) await Promise.resolve();
  };

  const healthy = (terminalId = "c0ffee01") => ({
    operationId: CORTANA_RECONCILE_OPERATION_ID,
    action: "keep",
    healthy: true,
    terminalId,
    identityId: "identity-cortana",
    generation: 3,
    degradedReason: null,
  });

  it("reconciles at startup and periodically without overlapping requests", async () => {
    vi.useFakeTimers();
    let finishFirst: ((value: unknown) => void) | undefined;
    const reconcile = vi
      .fn<() => Promise<unknown>>()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishFirst = resolve;
          }),
      )
      .mockResolvedValue(healthy());
    const monitor = createCortanaReconciliationMonitor({
      reconcile,
      onResult: vi.fn(),
      onError: vi.fn(),
      intervalMs: 1_000,
    });

    monitor.start();
    expect(reconcile).toHaveBeenCalledTimes(1);
    // The poll is SELF-RESCHEDULING (it re-arms only after an outcome is known,
    // so the failure backoff can widen the delay). Overlap is therefore
    // prevented by construction rather than by the trailing-run collapse: no
    // tick can even occur while a request is in flight. The trailing-run path
    // still exists for EVENT-driven runs and is covered by the next test.
    await vi.advanceTimersByTimeAsync(3_000);
    expect(reconcile).toHaveBeenCalledTimes(1);

    finishFirst?.(healthy());
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(1);

    // Healthy, so the next poll lands one plain interval later.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(2);

    monitor.stop();
    await vi.advanceTimersByTimeAsync(3_000);
    expect(reconcile).toHaveBeenCalledTimes(2);
  });

  it("backs off while Cortana stays unhealthy and recovers the cadence on health", async () => {
    vi.useFakeTimers();
    // A permanently-failing reconcile used to retry every 30s forever - observed
    // at 2,891 consecutive failures in one session, whose `wsl.exe` probe churn
    // showed up as app-wide input latency. Each failure must widen the delay.
    const reconcile = vi
      .fn<() => Promise<unknown>>()
      .mockRejectedValue(new Error("reconcile_cortana: wedged"));
    const monitor = createCortanaReconciliationMonitor({
      reconcile,
      onResult: vi.fn(),
      onError: vi.fn(),
      intervalMs: 1_000,
    });

    monitor.start();
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(1);

    // After 1 failure the next poll is 2x the interval, so the plain interval
    // alone must NOT produce another attempt.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(2);

    // After 2 failures: 4x.
    await vi.advanceTimersByTimeAsync(3_000);
    expect(reconcile).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(3);

    // An explicit user retry ignores the backoff and resets it.
    monitor.requestNow();
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(4);

    // Now succeed: the cadence must return to the plain interval immediately.
    reconcile.mockResolvedValue(healthy());
    await vi.advanceTimersByTimeAsync(2_000);
    const afterRecovery = reconcile.mock.calls.length;
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile.mock.calls.length).toBeGreaterThan(afterRecovery);

    monitor.stop();
  });

  it("treats a persistent DEGRADED result as unhealthy for backoff purposes", async () => {
    vi.useFakeTimers();
    // A degraded result is a successful round-trip that still means Cortana is
    // down, and the retry costs exactly the same WSL probe tree as a thrown
    // failure - so it must back off too, not poll hot forever.
    const degraded = {
      operationId: "op",
      action: "degraded" as const,
      healthy: false,
      terminalId: null,
      identityId: null,
      generation: 0,
      degradedReason: "prepared cleanup pending",
    };
    const reconcile = vi
      .fn<() => Promise<unknown>>()
      .mockResolvedValue(degraded);
    const monitor = createCortanaReconciliationMonitor({
      reconcile,
      onResult: vi.fn(),
      onError: vi.fn(),
      intervalMs: 1_000,
    });

    monitor.start();
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reconcile).toHaveBeenCalledTimes(2);

    monitor.stop();
  });

  it("reacts to terminal exit and collapses liveness signals into one trailing run", async () => {
    let finishRecovery: ((value: unknown) => void) | undefined;
    const reconcile = vi
      .fn<() => Promise<unknown>>()
      .mockResolvedValueOnce(healthy())
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishRecovery = resolve;
          }),
      )
      .mockResolvedValue(healthy("c0ffee02"));
    const onResult = vi.fn();
    const monitor = createCortanaReconciliationMonitor({
      reconcile,
      onResult,
      onError: vi.fn(),
      intervalMs: 60_000,
    });

    monitor.start();
    await flushPromises();
    monitor.observeTerminals({ c0ffee01: { state: "live" } });
    monitor.observeTerminals({ c0ffee01: { state: "exited" } });
    monitor.observeTerminals({ c0ffee01: { state: "error" } });
    expect(reconcile).toHaveBeenCalledTimes(2);

    finishRecovery?.(healthy("c0ffee02"));
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(3);
    expect(onResult).toHaveBeenLastCalledWith(expect.objectContaining({ terminalId: "c0ffee02" }));
    monitor.stop();
  });

  it("collapses manual retries into one trailing reconciliation", async () => {
    let finishFirst: ((value: unknown) => void) | undefined;
    const reconcile = vi
      .fn<() => Promise<unknown>>()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishFirst = resolve;
          }),
      )
      .mockResolvedValue(healthy());
    const monitor = createCortanaReconciliationMonitor({
      reconcile,
      onResult: vi.fn(),
      onError: vi.fn(),
      intervalMs: 60_000,
    });

    monitor.start();
    monitor.requestNow();
    monitor.requestNow();
    expect(reconcile).toHaveBeenCalledTimes(1);

    finishFirst?.(healthy());
    await flushPromises();
    expect(reconcile).toHaveBeenCalledTimes(2);
    monitor.stop();
  });

  it("does not report results or schedule recovery after stop", async () => {
    let finish: ((value: unknown) => void) | undefined;
    const onResult = vi.fn();
    const monitor = createCortanaReconciliationMonitor({
      reconcile: () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
      onResult,
      onError: vi.fn(),
      intervalMs: 60_000,
    });

    monitor.start();
    monitor.stop();
    finish?.(healthy());
    await flushPromises();
    expect(onResult).not.toHaveBeenCalled();
  });
});
