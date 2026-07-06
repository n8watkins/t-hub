// Announce-on-attention gating tests: OFF by default (opt-in), never speaks
// while the master enable is off, speaks exactly once per transition INTO a
// needs-input state, a needs-input-to-needs-input flip stays quiet, bursts
// debounce to one cue per window, and transitions that happened while the
// feature was off never replay when it turns on.
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
}));
vi.mock("./voiceAudio", () => ({
  playWavBase64: vi.fn(),
}));

import { synthesizeVoice } from "../ipc/voice";
import { playWavBase64 } from "./voiceAudio";
import {
  handleStatusesChange,
  _resetVoiceAnnounceForTest,
  ANNOUNCE_MIN_GAP_MS,
} from "./voiceAnnounce";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";
import { useSupervision } from "../store/supervision";
import { useWorkspace } from "../store/workspace";
import type { SessionStatus } from "../ipc/model";

/** Flush the synthesize->play promise chain. */
async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function statuses(map: Record<string, SessionStatus>): Record<string, SessionStatus> {
  return { ...map };
}

beforeEach(() => {
  vi.mocked(synthesizeVoice).mockClear();
  vi.mocked(playWavBase64).mockClear();
  _resetVoiceAnnounceForTest();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    enabled: true,
    announceOnAttention: true,
    voice: "v.onnx",
    volume: 0.7,
    loaded: true,
  });
  useSupervision.setState({
    trees: {},
    statuses: {},
    snapshots: {},
    sessionIdByTmux: { th_cap00001: "sess-1" },
  });
  useWorkspace.setState({
    terminals: {
      cap00001: {
        id: "cap00001",
        tmuxSession: "th_cap00001",
        cwd: "/tmp/ship",
        title: "captain",
        state: "live",
      },
    },
    labels: {},
  });
});

describe("announce gating", () => {
  it("is OFF by default (announceOnAttention defaults false)", async () => {
    useVoice.setState({ announceOnAttention: DEFAULT_VOICE_SETTINGS.announceOnAttention });
    expect(useVoice.getState().announceOnAttention).toBe(false);
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("never speaks while the master enable is off", async () => {
    useVoice.setState({ enabled: false });
    handleStatusesChange(statuses({ "sess-1": "needsQuestion" }), 0);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("speaks once on a transition INTO needs-input, with label + voice + volume", async () => {
    handleStatusesChange(statuses({ "sess-1": "working" }), 0);
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission" }),
      ANNOUNCE_MIN_GAP_MS,
    );
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    const [text, voice] = vi.mocked(synthesizeVoice).mock.calls[0];
    expect(text).toMatch(/needs your attention$/);
    expect(text).toContain("captain"); // deriveLabel over the tile title
    expect(voice).toBe("v.onnx");
    expect(playWavBase64).toHaveBeenCalledWith("d2F2", 0.7);
  });

  it("stays quiet on a needs-input to needs-input flip (already alerted)", async () => {
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    handleStatusesChange(
      statuses({ "sess-1": "needsQuestion" }),
      ANNOUNCE_MIN_GAP_MS * 2,
    );
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
  });

  it("debounces a burst: at most one cue per gap window", async () => {
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    // A second session flips moments later - inside the window: quiet.
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission", "sess-2": "needsQuestion" }),
      1000,
    );
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    // After the window, a fresh transition speaks again.
    handleStatusesChange(
      statuses({ "sess-1": "working", "sess-2": "completed" }),
      2000,
    );
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission", "sess-2": "completed" }),
      ANNOUNCE_MIN_GAP_MS + 2000,
    );
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(2);
  });

  it("does not replay transitions that happened while announcements were off", async () => {
    useVoice.setState({ announceOnAttention: false });
    // The session flips to needs-input while the feature is off...
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
    // ...then the user opts in; the SAME state arriving again must stay quiet
    // (it is not a new transition).
    useVoice.setState({ announceOnAttention: true });
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission" }),
      ANNOUNCE_MIN_GAP_MS,
    );
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("ignores non-needs-input statuses (working, rateLimited, completed)", async () => {
    handleStatusesChange(statuses({ "sess-1": "working" }), 0);
    handleStatusesChange(statuses({ "sess-1": "rateLimited" }), 1);
    handleStatusesChange(statuses({ "sess-1": "completed" }), 2);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });
});
