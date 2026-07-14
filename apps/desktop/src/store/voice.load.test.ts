import { describe, expect, it, vi } from "vitest";
import type { VoiceSettings } from "../ipc/voice";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  voiceHealth: vi.fn(),
}));

import { readVoiceSettings } from "../ipc/voice";
import { useVoice } from "./voice";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function fileSettings(overrides: Partial<VoiceSettings>): VoiceSettings {
  return {
    enabled: false,
    engine: "piper",
    voice: "file.onnx",
    volume: 0.25,
    sapiRate: 0,
    announceOnAttention: false,
    ...overrides,
  };
}

describe("voice settings hydration", () => {
  it("ignores stale loads and preserves fields changed during the latest read", async () => {
    vi.useFakeTimers();
    const first = deferred<VoiceSettings>();
    const second = deferred<VoiceSettings>();
    const third = deferred<VoiceSettings>();
    vi.mocked(readVoiceSettings)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
      .mockReturnValueOnce(third.promise);

    const loadOne = useVoice.getState().load();
    const loadTwo = useVoice.getState().load();
    second.resolve(fileSettings({ volume: 0.4, voice: "newer.onnx" }));
    await loadTwo;
    first.resolve(fileSettings({ volume: 0.1, voice: "stale.onnx" }));
    await loadOne;
    expect(useVoice.getState().voice).toBe("newer.onnx");

    const loadThree = useVoice.getState().load();
    useVoice.getState().setEnabled(true);
    useVoice.getState().setAnnounceOnAttention(true);
    // Let the debounce persist and clear dirtyFields before the older load
    // resolves. Per-field generations must still protect the user changes.
    await vi.advanceTimersByTimeAsync(300);
    third.resolve(fileSettings({ volume: 0.6 }));
    await loadThree;

    expect(useVoice.getState()).toMatchObject({
      enabled: true,
      announceOnAttention: true,
      volume: 0.6,
      loaded: true,
    });
    vi.clearAllTimers();
    vi.useRealTimers();
  });
});
