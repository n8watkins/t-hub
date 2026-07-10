// Settings > Voice section tests: the engine selector (switching engines
// re-queries that engine's /voices and self-heals a foreign voice), the
// /voices degradation contract (server down = every control disabled EXCEPT the
// master enable toggle AND the engine selector, so the user can switch engines
// while one is offline), the dual-engine HEALTH display + selected-engine-down
// error state (never a silent default), the healthy path (voice list populates,
// Test synthesizes with the selected voice + engine), and recovery.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
  voiceHealth: vi.fn(),
}));
vi.mock("../lib/voiceAudio", () => ({
  playWavBase64: vi.fn(),
}));

import {
  readVoiceSettings,
  listVoices,
  synthesizeVoice,
  voiceHealth,
  type EngineHealth,
  type VoiceEngine,
  type VoiceSettings as VoiceSettingsShape,
} from "../ipc/voice";
import { playWavBase64 } from "../lib/voiceAudio";
import { VoiceSection, VOICE_TEST_PHRASE } from "./VoiceSettings";
import {
  useVoice,
  DEFAULT_VOICE_SETTINGS,
  _resetVoicePersistForTest,
} from "../store/voice";

const FILE_SETTINGS: VoiceSettingsShape = {
  enabled: true,
  engine: "piper",
  voice: "en_US-ryan-high.onnx",
  volume: 0.8,
  sapiRate: 0,
  announceOnAttention: false,
};

const PIPER_VOICES = ["en_US-ryan-high.onnx", "en_US-lessac-medium.onnx"];
const KOKORO_VOICES = ["af_heart", "am_adam"];

/** Build an EngineHealth reply for the voiceHealth mock. */
function health(engine: VoiceEngine, reachable: boolean): EngineHealth {
  return { engine, reachable, detail: reachable ? null : "connection refused" };
}
/** A voiceHealth mock where each engine reports the given reachability. */
function mockHealth(byEngine: Record<VoiceEngine, boolean>) {
  vi.mocked(voiceHealth).mockImplementation((engine: VoiceEngine) =>
    Promise.resolve(health(engine, byEngine[engine])),
  );
}

/** Dropdowns in DOM order: [0] = Engine, [1] = Voice. */
function engineSelect(): HTMLSelectElement {
  return screen.getAllByRole("combobox")[0] as HTMLSelectElement;
}
function voiceSelect(): HTMLSelectElement {
  return screen.getAllByRole("combobox")[1] as HTMLSelectElement;
}

beforeEach(() => {
  vi.mocked(readVoiceSettings).mockReset();
  vi.mocked(listVoices).mockReset();
  vi.mocked(voiceHealth).mockReset();
  vi.mocked(synthesizeVoice).mockClear();
  vi.mocked(playWavBase64).mockClear();
  _resetVoicePersistForTest();
  vi.mocked(readVoiceSettings).mockResolvedValue(FILE_SETTINGS);
  // Default: both engines reachable (the health probe fires on mount). Cases
  // that test a down engine override this.
  mockHealth({ piper: true, kokoro: true });
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,
    health: { piper: "unknown", kokoro: "unknown" },
  });
});

describe("VoiceSection degradation (selected engine down)", () => {
  beforeEach(() => {
    // Selected engine (Piper, per FILE_SETTINGS) is down on both probes.
    vi.mocked(listVoices).mockRejectedValue(new Error("connection refused"));
    mockHealth({ piper: false, kokoro: false });
  });

  it("raises the selected-engine-down error and disables everything except Enable + Engine", async () => {
    render(<VoiceSection />);
    // The prominent, assertive error - announcements will NOT be spoken.
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/unreachable/i);
    expect(alert.textContent).toMatch(/will NOT be spoken/i);
    expect(voiceSelect()).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Volume")).toHaveProperty("disabled", true);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", true);
    expect(
      screen.getByLabelText("Announce when a session needs attention"),
    ).toHaveProperty("disabled", true);
    // The master switch + engine selector stay interactive so the user can
    // flip intent or switch to the other engine while one is down.
    expect(screen.getByLabelText("Enable voice")).toHaveProperty(
      "disabled",
      false,
    );
    expect(engineSelect()).toHaveProperty("disabled", false);
  });

  it("still shows the persisted voice in the (disabled) select", async () => {
    render(<VoiceSection />);
    await screen.findByRole("alert");
    expect(
      screen.getByRole("option", { name: "en_US-ryan-high.onnx" }),
    ).toBeTruthy();
  });
});

describe("VoiceSection with the server up", () => {
  beforeEach(() => {
    vi.mocked(listVoices).mockResolvedValue(PIPER_VOICES);
  });

  it("populates the voice dropdown from /voices and enables controls", async () => {
    render(<VoiceSection />);
    expect(
      await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" }),
    ).toBeTruthy();
    expect(screen.queryByText(/server unavailable/)).toBeNull();
    expect(voiceSelect()).toHaveProperty("disabled", false);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", false);
  });

  it("Test synthesizes the phrase with the selected voice AND engine, then plays it", async () => {
    render(<VoiceSection />);
    await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" });
    fireEvent.click(screen.getByRole("button", { name: /Test voice/ }));
    expect(synthesizeVoice).toHaveBeenCalledWith(
      VOICE_TEST_PHRASE,
      "en_US-ryan-high.onnx",
      "piper",
    );
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
    expect(voiceSelect()).toHaveProperty("disabled", true);
    expect(
      screen.getByRole("button", { name: /Test voice/ }),
    ).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Enable voice")).toHaveProperty(
      "disabled",
      false,
    );
  });
});

describe("VoiceSection engine switching", () => {
  beforeEach(() => {
    // Per-engine voice lists so a switch visibly changes the dropdown.
    vi.mocked(listVoices).mockImplementation((engine: VoiceEngine) =>
      Promise.resolve(engine === "kokoro" ? KOKORO_VOICES : PIPER_VOICES),
    );
  });

  it("switching the engine dropdown re-queries that engine and swaps the voice list", async () => {
    render(<VoiceSection />);
    // Starts on Piper.
    await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" });
    expect(listVoices).toHaveBeenCalledWith("piper");

    // Switch to Kokoro via the engine dropdown.
    fireEvent.change(engineSelect(), { target: { value: "kokoro" } });

    // The Kokoro voices load and appear; the Piper ones are gone.
    expect(await screen.findByRole("option", { name: "af_heart" })).toBeTruthy();
    expect(listVoices).toHaveBeenLastCalledWith("kokoro");
    expect(
      screen.queryByRole("option", { name: "en_US-lessac-medium.onnx" }),
    ).toBeNull();
    expect(useVoice.getState().engine).toBe("kokoro");
  });

  it("self-heals a foreign voice to the first voice of the newly selected engine", async () => {
    render(<VoiceSection />);
    await screen.findByRole("option", { name: "en_US-lessac-medium.onnx" });
    // Piper voice persisted; switch to Kokoro whose list lacks it.
    fireEvent.change(engineSelect(), { target: { value: "kokoro" } });
    // The self-heal effect adopts the first Kokoro voice so Test/announce
    // target a real voice.
    await waitFor(() =>
      expect(useVoice.getState().voice).toBe("af_heart"),
    );
  });
});

describe("VoiceSection engine health (never a silent default)", () => {
  it("shows BOTH engines' reachability, and no error when the SELECTED engine is up", async () => {
    // Selected = Piper (up); the OTHER engine (Kokoro) is down.
    vi.mocked(listVoices).mockResolvedValue(PIPER_VOICES);
    mockHealth({ piper: true, kokoro: false });
    render(<VoiceSection />);

    const status = await screen.findByLabelText("TTS engine health");
    await waitFor(() => {
      expect(status.textContent).toMatch(/Piper.*reachable/);
      expect(status.textContent).toMatch(/Kokoro.*unreachable/);
    });
    // The selected engine is fine, so no assertive error is raised even though
    // the other engine is down.
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("suggests switching to the other engine when the selected one is down but the other is up", async () => {
    // Selected = Kokoro (down); Piper is up - the alert should offer the switch.
    vi.mocked(readVoiceSettings).mockResolvedValue({
      ...FILE_SETTINGS,
      engine: "kokoro",
      voice: "af_heart",
    });
    vi.mocked(listVoices).mockRejectedValue(new Error("connection refused"));
    mockHealth({ piper: true, kokoro: false });
    render(<VoiceSection />);

    const alert = await screen.findByRole("alert");
    await waitFor(() => expect(alert.textContent).toMatch(/switch to piper/i));
    expect(alert.textContent).toMatch(/kokoro.*unreachable/i);
  });

  it("does not render the health block while voice is disabled", async () => {
    vi.mocked(readVoiceSettings).mockResolvedValue({
      ...FILE_SETTINGS,
      enabled: false,
    });
    vi.mocked(listVoices).mockResolvedValue(PIPER_VOICES);
    mockHealth({ piper: false, kokoro: false });
    render(<VoiceSection />);
    // Master off = the whole health/error region is suppressed (dependent UI).
    await waitFor(() =>
      expect(screen.getByLabelText("Enable voice")).toHaveProperty(
        "disabled",
        false,
      ),
    );
    expect(screen.queryByLabelText("TTS engine health")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
