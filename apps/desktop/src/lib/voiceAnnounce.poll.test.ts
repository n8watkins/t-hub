import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  voiceHealth: vi.fn(),
  synthesizeVoice: vi.fn(),
}));

vi.mock("../ipc/scribe", () => ({
  scribeStatus: vi.fn(() => Promise.resolve({ listening: false })),
}));

import { scribeStatus } from "../ipc/scribe";
import { DEFAULT_VOICE_SETTINGS, useVoice } from "../store/voice";
import {
  SCRIBE_POLL_MS,
  _resetVoiceAnnounceForTest,
  startScribePoll,
} from "./voiceAnnounce";

beforeEach(() => {
  vi.useFakeTimers();
  _resetVoiceAnnounceForTest();
  vi.mocked(scribeStatus).mockReset();
  vi.mocked(scribeStatus).mockResolvedValue({ listening: false });
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: true,
  });
});

describe("Scribe poll lifecycle", () => {
  it("does not call Scribe while voice announcements are disabled", async () => {
    startScribePoll();

    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 4);

    expect(scribeStatus).not.toHaveBeenCalled();
  });

  it("starts one poller when required and stops it when either gate turns off", async () => {
    startScribePoll();
    useVoice.setState({ enabled: true, announceOnAttention: true });
    await Promise.resolve();

    expect(scribeStatus).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 3);
    expect(scribeStatus).toHaveBeenCalledTimes(4);

    useVoice.setState({ volume: 0.4 });
    startScribePoll();
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS);
    expect(scribeStatus).toHaveBeenCalledTimes(5);

    useVoice.setState({ announceOnAttention: false });
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 4);
    expect(scribeStatus).toHaveBeenCalledTimes(5);

    useVoice.setState({ announceOnAttention: true });
    await Promise.resolve();
    expect(scribeStatus).toHaveBeenCalledTimes(6);

    useVoice.setState({ enabled: false });
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 4);
    expect(scribeStatus).toHaveBeenCalledTimes(6);
  });

  it("ignores an in-flight result after the poller is disabled", async () => {
    let resolveStatus: ((status: { listening: boolean }) => void) | undefined;
    vi.mocked(scribeStatus).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveStatus = resolve;
        }),
    );
    startScribePoll();
    useVoice.setState({ enabled: true, announceOnAttention: true });
    expect(scribeStatus).toHaveBeenCalledTimes(1);

    useVoice.setState({ enabled: false });
    resolveStatus?.({ listening: true });
    await Promise.resolve();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 2);

    expect(scribeStatus).toHaveBeenCalledTimes(1);
  });
});
