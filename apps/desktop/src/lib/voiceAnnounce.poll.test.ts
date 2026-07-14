import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  voiceHealth: vi.fn(),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
}));
vi.mock("./voiceAudio", () => ({ playWavBase64: vi.fn() }));

vi.mock("../ipc/scribe", () => ({
  scribeStatus: vi.fn(() => Promise.resolve({ listening: false })),
}));

import { scribeStatus } from "../ipc/scribe";
import { synthesizeVoice } from "../ipc/voice";
import { DEFAULT_VOICE_SETTINGS, useVoice } from "../store/voice";
import { useSupervision } from "../store/supervision";
import {
  SCRIBE_POLL_MS,
  SCRIBE_TAIL_MS,
  _pendingTextForTest,
  _resetVoiceAnnounceForTest,
  handleStatusesChange,
  startScribePoll,
} from "./voiceAnnounce";

beforeEach(() => {
  vi.useFakeTimers();
  _resetVoiceAnnounceForTest();
  vi.mocked(scribeStatus).mockReset();
  vi.mocked(scribeStatus).mockResolvedValue({ listening: false });
  vi.mocked(synthesizeVoice).mockClear();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: true,
  });
  useSupervision.setState({ statuses: { session: "working" } });
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

  it("holds a cue until the first status confirms Scribe is idle", async () => {
    let resolveStatus: ((status: { listening: boolean }) => void) | undefined;
    vi.mocked(scribeStatus).mockImplementationOnce(
      () => new Promise((resolve) => { resolveStatus = resolve; }),
    );
    startScribePoll();
    useVoice.setState({ enabled: true, announceOnAttention: true });

    const blocked = { session: "needsPermission" as const };
    useSupervision.setState({ statuses: blocked });
    handleStatusesChange(blocked);
    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(_pendingTextForTest()).not.toBeNull();

    resolveStatus?.({ listening: false });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(SCRIBE_TAIL_MS);
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
  });

  it("ignores an old generation when disable and re-enable resolve out of order", async () => {
    const resolvers: Array<(status: { listening: boolean }) => void> = [];
    vi.mocked(scribeStatus).mockImplementation(
      () => new Promise((resolve) => { resolvers.push(resolve); }),
    );
    startScribePoll();
    useVoice.setState({ enabled: true, announceOnAttention: true });
    useVoice.setState({ enabled: false });
    useVoice.setState({ enabled: true });
    expect(resolvers).toHaveLength(2);

    const blocked = { session: "needsQuestion" as const };
    useSupervision.setState({ statuses: blocked });
    handleStatusesChange(blocked);
    resolvers[1]({ listening: true });
    resolvers[0]({ listening: false });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(SCRIBE_TAIL_MS * 2);

    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(_pendingTextForTest()).not.toBeNull();
  });

  it("fails open only after the first IPC failure settles", async () => {
    let rejectStatus: ((error: Error) => void) | undefined;
    vi.mocked(scribeStatus).mockImplementationOnce(
      () => new Promise((_resolve, reject) => { rejectStatus = reject; }),
    );
    startScribePoll();
    useVoice.setState({ enabled: true, announceOnAttention: true });
    const blocked = { session: "needsPermission" as const };
    useSupervision.setState({ statuses: blocked });
    handleStatusesChange(blocked);
    expect(synthesizeVoice).not.toHaveBeenCalled();

    rejectStatus?.(new Error("offline"));
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(SCRIBE_TAIL_MS);
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
  });
});
