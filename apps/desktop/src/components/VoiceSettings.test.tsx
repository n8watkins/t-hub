// Settings > Voice section tests: the /voices degradation contract (server
// down = unavailable hint + every control disabled EXCEPT the master enable
// toggle), the healthy path (voice list populates, Test synthesizes with the
// selected voice), and recovery when the server comes back between mounts.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
}));
vi.mock("../lib/voiceAudio", () => ({
  playWavBase64: vi.fn(),
}));

import {
  readVoiceSettings,
  listVoices,
  synthesizeVoice,
  type VoiceSettings as VoiceSettingsShape,
} from "../ipc/voice";
import { playWavBase64 } from "../lib/voiceAudio";
import { VoiceSection, VOICE_TEST_PHRASE } from "./VoiceSettings";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";

const FILE_SETTINGS: VoiceSettingsShape = {
  enabled: true,
  voice: "en_US-ryan-high.onnx",
  volume: 0.8,
  sapiRate: 0,
  announceOnAttention: false,
};

beforeEach(() => {
  vi.mocked(readVoiceSettings).mockReset();
  vi.mocked(listVoices).mockReset();
  vi.mocked(synthesizeVoice).mockClear();
  vi.mocked(playWavBase64).mockClear();
  vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,
  });
});

describe("VoiceSection degradation (server down)", () => {
  beforeEach(() => {
    vi.mocked(listVoices).mockRejectedValue(new Error("connection refused"));
  });

  it("shows the unavailable hint and disables everything except Enable", async () => {
    render(<VoiceSection />);
    // The mount effect loads the file then probes /voices, which fails.
    expect(
      await screen.findByText(/Voice server unavailable/),
    ).toBeTruthy();
    expect(screen.getByRole("combobox")).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Volume")).toHaveProperty("disabled", true);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", true);
    expect(
      screen.getByLabelText("Announce when a session needs attention"),
    ).toHaveProperty("disabled", true);
    // The master switch stays interactive so intent can still be flipped.
    expect(screen.getByLabelText("Enable voice")).toHaveProperty(
      "disabled",
      false,
    );
  });

  it("still shows the persisted voice in the (disabled) select", async () => {
    render(<VoiceSection />);
    await screen.findByText(/Voice server unavailable/);
    expect(
      screen.getByRole("option", { name: "en_US-ryan-high.onnx" }),
    ).toBeTruthy();
  });
});

describe("VoiceSection with the server up", () => {
  beforeEach(() => {
    vi.mocked(listVoices).mockResolvedValue([
      "en_US-ryan-high.onnx",
      "en_US-lessac-medium.onnx",
    ]);
  });

  it("populates the voice dropdown from /voices and enables controls", async () => {
    render(<VoiceSection />);
    expect(
      await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" }),
    ).toBeTruthy();
    expect(screen.queryByText(/Voice server unavailable/)).toBeNull();
    expect(screen.getByRole("combobox")).toHaveProperty("disabled", false);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", false);
  });

  it("Test synthesizes the phrase with the selected voice and plays it", async () => {
    render(<VoiceSection />);
    await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" });
    fireEvent.click(screen.getByRole("button", { name: /Test voice/ }));
    expect(synthesizeVoice).toHaveBeenCalledWith(
      VOICE_TEST_PHRASE,
      "en_US-ryan-high.onnx",
    );
    // Playback happens after the synth promise resolves.
    expect(await screen.findByRole("button", { name: /Test voice/ })).toBeTruthy();
    expect(playWavBase64).toHaveBeenCalledWith("d2F2", 0.8);
  });

  it("controls dim while the master enable is off (dependent settings)", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue({
      ...FILE_SETTINGS,
      enabled: false,
    });
    render(<VoiceSection />);
    await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" });
    expect(screen.getByRole("combobox")).toHaveProperty("disabled", true);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Enable voice")).toHaveProperty(
      "disabled",
      false,
    );
  });
});
