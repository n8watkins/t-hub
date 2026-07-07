// Voice store tests (Settings > Voice): the persistence round-trip through
// the (mocked) Tauri command seam - voice.json is the source of truth, no
// localStorage - the debounced READ-MERGE-WRITE (external edits to fields the
// UI did not touch survive a persist), the load-vs-pending-persist guard, and
// the /voices degradation flags the Settings section renders from.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  synthesizeVoice: vi.fn(),
}));

import {
  readVoiceSettings,
  writeVoiceSettings,
  listVoices,
  type VoiceSettings,
} from "../ipc/voice";
import {
  useVoice,
  DEFAULT_VOICE_SETTINGS,
  VOICE_PERSIST_DEBOUNCE_MS,
  _resetVoicePersistForTest,
} from "./voice";

const FILE_SETTINGS: VoiceSettings = {
  enabled: true,
  engine: "piper",
  voice: "en_US-lessac-medium.onnx",
  volume: 0.55,
  sapiRate: 2,
  announceOnAttention: true,
};

/** Advance past the debounce and flush the async read-merge-write chain. */
async function flushPersist(): Promise<void> {
  await vi.advanceTimersByTimeAsync(VOICE_PERSIST_DEBOUNCE_MS);
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(readVoiceSettings).mockReset();
  vi.mocked(writeVoiceSettings).mockClear();
  vi.mocked(listVoices).mockReset();
  _resetVoicePersistForTest();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,
  });
});

afterEach(async () => {
  // Flush any pending debounced write so it can't leak into the next case.
  await vi.runAllTimersAsync();
  vi.useRealTimers();
});

describe("voice settings round-trip", () => {
  it("load() hydrates every field from the file, including sapiRate", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
    await useVoice.getState().load();
    const s = useVoice.getState();
    expect(s.loaded).toBe(true);
    expect(s.enabled).toBe(true);
    expect(s.voice).toBe("en_US-lessac-medium.onnx");
    expect(s.volume).toBe(0.55);
    expect(s.sapiRate).toBe(2);
    expect(s.announceOnAttention).toBe(true);
  });

  it("load() falls back to defaults when the read command fails", async () => {
    vi.mocked(readVoiceSettings).mockRejectedValue(new Error("no backend"));
    await useVoice.getState().load();
    const s = useVoice.getState();
    expect(s.loaded).toBe(true);
    expect(s.enabled).toBe(DEFAULT_VOICE_SETTINGS.enabled);
    expect(s.voice).toBe(DEFAULT_VOICE_SETTINGS.voice);
  });

  it("a setter writes the FULL schema back (round-trips foreign sapiRate)", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
    await useVoice.getState().load();

    useVoice.getState().setVolume(0.3);
    expect(writeVoiceSettings).not.toHaveBeenCalled(); // debounced
    await flushPersist();
    expect(writeVoiceSettings).toHaveBeenCalledTimes(1);
    expect(writeVoiceSettings).toHaveBeenCalledWith({
      enabled: true,
      engine: "piper",
      voice: "en_US-lessac-medium.onnx",
      volume: 0.3,
      sapiRate: 2, // untouched by the UI, faithfully round-tripped
      announceOnAttention: true,
    });
  });

  it("a burst of setter calls coalesces into ONE debounced write", async () => {
    useVoice.getState().setVolume(0.1);
    useVoice.getState().setVolume(0.2);
    useVoice.getState().setEnabled(true);
    useVoice.getState().setVoice("x.onnx");
    await flushPersist();
    expect(writeVoiceSettings).toHaveBeenCalledTimes(1);
    expect(writeVoiceSettings).toHaveBeenCalledWith(
      expect.objectContaining({ enabled: true, voice: "x.onnx", volume: 0.2 }),
    );
  });

  it("setVolume clamps into 0..=1", () => {
    useVoice.getState().setVolume(4);
    expect(useVoice.getState().volume).toBe(1);
    useVoice.getState().setVolume(-1);
    expect(useVoice.getState().volume).toBe(0);
  });
});

describe("external edits (shared file)", () => {
  it("persist is read-MERGE-write: an external flip of a non-dirty field survives", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
    await useVoice.getState().load();
    expect(useVoice.getState().enabled).toBe(true);

    // A captain script flips enabled OFF in the file behind our back...
    vi.mocked(readVoiceSettings).mockResolvedValue({
      ...FILE_SETTINGS,
      enabled: false,
    });
    // ...then the user drags the volume slider (only `volume` is dirty).
    useVoice.getState().setVolume(0.3);
    await flushPersist();

    // The write must carry the FILE's enabled:false, not resurrect our stale
    // true - and the live store adopts the external value too.
    expect(writeVoiceSettings).toHaveBeenCalledWith(
      expect.objectContaining({ enabled: false, volume: 0.3 }),
    );
    expect(useVoice.getState().enabled).toBe(false);
    expect(useVoice.getState().volume).toBe(0.3);
  });

  it("load() is skipped while a persist is pending (no stale clobber)", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
    await useVoice.getState().load();

    useVoice.getState().setVolume(0.9);
    // Settings re-mount triggers a load within the debounce window: it must
    // NOT overwrite the un-persisted 0.9 with the file's 0.55.
    await useVoice.getState().load();
    expect(useVoice.getState().volume).toBe(0.9);

    await flushPersist();
    expect(writeVoiceSettings).toHaveBeenCalledWith(
      expect.objectContaining({ volume: 0.9 }),
    );
  });
});

describe("voices endpoint degradation", () => {
  it("refreshVoices() success stores the list and clears unavailable", async () => {
    vi.mocked(listVoices).mockResolvedValue(["a.onnx", "b.onnx"]);
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voices).toEqual(["a.onnx", "b.onnx"]);
    expect(useVoice.getState().voicesUnavailable).toBe(false);
  });

  it("refreshVoices() queries the SELECTED engine", async () => {
    vi.mocked(listVoices).mockResolvedValue(["af_heart"]);
    useVoice.setState({ engine: "kokoro" });
    await useVoice.getState().refreshVoices();
    expect(listVoices).toHaveBeenCalledWith("kokoro");
  });

  it("refreshVoices() failure flips voicesUnavailable (server down)", async () => {
    vi.mocked(listVoices).mockRejectedValue(new Error("connection refused"));
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voices).toBeNull();
    expect(useVoice.getState().voicesUnavailable).toBe(true);
  });

  it("recovers when the server comes back", async () => {
    vi.mocked(listVoices).mockRejectedValueOnce(new Error("down"));
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voicesUnavailable).toBe(true);
    vi.mocked(listVoices).mockResolvedValue(["a.onnx"]);
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voicesUnavailable).toBe(false);
    expect(useVoice.getState().voices).toEqual(["a.onnx"]);
  });
});

describe("engine switching", () => {
  it("load() round-trips the engine from the file (kokoro)", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue({
      ...FILE_SETTINGS,
      engine: "kokoro",
      voice: "af_heart",
    });
    await useVoice.getState().load();
    expect(useVoice.getState().engine).toBe("kokoro");
    expect(useVoice.getState().voice).toBe("af_heart");
  });

  it("setEngine switches, drops the stale voice list, and persists the engine", async () => {
    // Start on Piper with a loaded list.
    vi.mocked(listVoices).mockResolvedValue(["en_US-ryan-high.onnx"]);
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voices).toEqual(["en_US-ryan-high.onnx"]);

    // Switch to Kokoro: the stale list is dropped synchronously (the NEW
    // engine's list is loaded by the Settings section's engine effect, tested
    // in VoiceSettings.test.tsx - the store action itself stays pure).
    useVoice.getState().setEngine("kokoro");
    expect(useVoice.getState().engine).toBe("kokoro");
    expect(useVoice.getState().voices).toBeNull();

    // The engine change persists through the merge-write.
    await flushPersist();
    expect(writeVoiceSettings).toHaveBeenCalledWith(
      expect.objectContaining({ engine: "kokoro" }),
    );
  });

  it("setEngine to the SAME engine is a no-op (no re-query, no dirty)", async () => {
    vi.mocked(listVoices).mockClear();
    useVoice.getState().setEngine("piper"); // already piper
    expect(listVoices).not.toHaveBeenCalled();
    await flushPersist();
    expect(writeVoiceSettings).not.toHaveBeenCalled();
  });
});
