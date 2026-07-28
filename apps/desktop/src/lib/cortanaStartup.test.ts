import { describe, expect, it, vi } from "vitest";
import {
  createCortanaRecoveryOperation,
  cortanaFailureMessage,
  isAmbiguousCortanaFailure,
  newCortanaRecoveryId,
} from "./cortanaStartup";

describe("Cortana startup recovery", () => {
  it("reuses only ambiguous request identities", () => {
    const ids = ["operation-1", "operation-2", "operation-3"];
    const operation = createCortanaRecoveryOperation(() => ids.shift() ?? "unexpected");

    expect(operation.currentId()).toBe("operation-1");
    operation.failure("control_timeout: response was lost");
    expect(operation.currentId()).toBe("operation-1");
    operation.authoritativeResult();
    expect(operation.currentId()).toBe("operation-2");
    operation.failure(new Error("durable identity is invalid"));
    expect(operation.currentId()).toBe("operation-3");
  });

  it("preserves the operation identity after ambiguous transport failures", () => {
    expect(isAmbiguousCortanaFailure("control_timeout: response was lost")).toBe(true);
    expect(isAmbiguousCortanaFailure("control_unavailable: endpoint rotated")).toBe(true);
    expect(isAmbiguousCortanaFailure("request 'same-id' is already in flight")).toBe(true);
    expect(isAmbiguousCortanaFailure({ retryable: true, message: "bridge reset" })).toBe(true);
  });

  it("keeps durable operation ownership through inconclusive retirement evidence", () => {
    const ids = ["durable-retirement", "next-health-check"];
    const operation = createCortanaRecoveryOperation(() => ids.shift() ?? "conflicting-operation");

    operation.failure({
      retryable: true,
      message: "prepared managed unit was unverifiable",
    });
    expect(operation.currentId()).toBe("durable-retirement");

    operation.failure({
      retryable: true,
      message: "exact tmux generation liveness is indeterminate",
    });
    expect(operation.currentId()).toBe("durable-retirement");

    operation.authoritativeResult();
    expect(operation.currentId()).toBe("next-health-check");
  });

  it("rotates the operation identity after authoritative recovery failures", () => {
    expect(isAmbiguousCortanaFailure("Cortana recovery evidence is ambiguous")).toBe(false);
    expect(isAmbiguousCortanaFailure(new Error("unsupported durable harness"))).toBe(false);
  });

  it("bounds the diagnostic rendered in the startup alert", () => {
    const message = cortanaFailureMessage(`  ${"failure ".repeat(80)}  `);
    expect(message).toHaveLength(240);
    expect(message.endsWith("...")).toBe(true);
  });

  it("creates a local fallback identity when randomUUID is unavailable", () => {
    vi.stubGlobal("crypto", {});
    expect(newCortanaRecoveryId()).toMatch(/^cortana_[a-z0-9]+_[a-z0-9]+$/);
    vi.unstubAllGlobals();
  });
});
