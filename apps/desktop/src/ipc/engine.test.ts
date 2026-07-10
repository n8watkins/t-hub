// effectiveTarget is the wave-1 voice-remap correctness catch: on a managed
// fallback, synthesis must target the ACTIVE engine with a valid voice for it
// (the selected Kokoro voice would 400 on Piper), and otherwise pass the
// selected engine + voice through unchanged.
import { describe, it, expect } from "vitest";
import { effectiveTarget, DEFAULT_ENGINE_VOICE, type EngineRuntimeStatus } from "./engine";

function status(over: Partial<EngineRuntimeStatus>): EngineRuntimeStatus {
  return {
    managed: true,
    selectedEngine: "kokoro",
    activeEngine: "kokoro",
    degraded: false,
    level: "green",
    kokoro: "up",
    piper: "unknown",
    ...over,
  };
}

describe("effectiveTarget", () => {
  it("passes the selected engine + voice through when status is null (no backend)", () => {
    expect(effectiveTarget(null, "kokoro", "af_heart")).toEqual({
      engine: "kokoro",
      voice: "af_heart",
    });
  });

  it("passes through when unmanaged (flag off)", () => {
    const s = status({ managed: false, activeEngine: "piper", degraded: true });
    // Even though the snapshot shows piper, an unmanaged snapshot is not
    // authoritative - keep the user's selection.
    expect(effectiveTarget(s, "kokoro", "af_heart")).toEqual({
      engine: "kokoro",
      voice: "af_heart",
    });
  });

  it("passes through when managed but active == selected (healthy)", () => {
    const s = status({ activeEngine: "kokoro" });
    expect(effectiveTarget(s, "kokoro", "af_heart")).toEqual({
      engine: "kokoro",
      voice: "af_heart",
    });
  });

  it("remaps to the active engine + its default voice on fallback", () => {
    const s = status({ activeEngine: "piper", degraded: true, level: "amber" });
    expect(effectiveTarget(s, "kokoro", "af_heart")).toEqual({
      engine: "piper",
      voice: DEFAULT_ENGINE_VOICE.piper,
    });
    // af_heart (a Kokoro voice) must NOT leak to Piper.
    expect(effectiveTarget(s, "kokoro", "af_heart").voice).not.toBe("af_heart");
  });
});
