import { describe, expect, it, vi } from "vitest";
import {
  cortanaFailureMessage,
  isAmbiguousCortanaFailure,
  newCortanaRecoveryId,
} from "./cortanaStartup";

describe("Cortana startup recovery", () => {
  it("preserves the operation identity after ambiguous transport failures", () => {
    expect(isAmbiguousCortanaFailure("control_timeout: response was lost")).toBe(true);
    expect(isAmbiguousCortanaFailure("control_unavailable: endpoint rotated")).toBe(true);
    expect(isAmbiguousCortanaFailure("request 'same-id' is already in flight")).toBe(true);
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
