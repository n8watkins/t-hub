// Voice store tests (Settings > Voice): the persistence round-trip through
// the (mocked) Tauri command seam - voice.json is the source of truth, no
// localStorage - plus the debounced write coalescing and the /voices
// degradation flags the Settings section renders from.
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
} from "./voice";

const FILE_SETTINGS: VoiceSettings = {
  enabled: true,
  voice: "en_US-lessac-medium.onnx",
  volume: 0.55,
  sapiRate: 2,
  announceOnAttention: true,
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(readVoiceSettings).mockReset();
  vi.mocked(writeVoiceSettings).mockClear();
  vi.mocked(listVoices).mockReset();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,
  });
});

afterEach(() => {
  // Flush any pending debounced write so it can't leak into the next case.
  vi.runAllTimers();
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
    vi.advanceTimersByTime(VOICE_PERSIST_DEBOUNCE_MS);
    expect(writeVoiceSettings).toHaveBeenCalledTimes(1);
    expect(writeVoiceSettings).toHaveBeenCalledWith({
      enabled: true,
      voice: "en_US-lessac-medium.onnx",
      volume: 0.3,
      sapiRate: 2, // untouched by the UI, faithfully round-tripped
      announceOnAttention: true,
    });
  });

  it("a burst of setter calls coalesces into ONE debounced write", () => {
    useVoice.getState().setVolume(0.1);
    useVoice.getState().setVolume(0.2);
    useVoice.getState().setEnabled(true);
    useVoice.getState().setVoice("x.onnx");
    vi.advanceTimersByTime(VOICE_PERSIST_DEBOUNCE_MS);
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

describe("voices endpoint degradation", () => {
  it("refreshVoices() success stores the list and clears unavailable", async () => {
    vi.mocked(listVoices).mockResolvedValue(["a.onnx", "b.onnx"]);
    await useVoice.getState().refreshVoices();
    expect(useVoice.getState().voices).toEqual(["a.onnx", "b.onnx"]);
    expect(useVoice.getState().voicesUnavailable).toBe(false);
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
