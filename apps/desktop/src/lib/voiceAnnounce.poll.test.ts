import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  readVoiceSettings: vi.fn(),
  writeVoiceSettings: vi.fn(() => Promise.resolve()),
  listVoices: vi.fn(),
  voiceHealth: vi.fn(),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
  recordVoiceAnnouncementOutcome: vi.fn(() => Promise.resolve()),
  recoverVoiceAnnouncements: vi.fn(() => Promise.resolve(null)),
}));
vi.mock("./voiceAudio", () => ({ playWavBase64: vi.fn() }));

vi.mock("../ipc/scribe", () => ({
  scribeStatus: vi.fn(() => Promise.resolve({ listening: false })),
  onScribeStatus: vi.fn(() => Promise.resolve(() => {})),
  startScribeStatusEmitter: vi.fn(() => Promise.resolve()),
  stopScribeStatusEmitter: vi.fn(() => Promise.resolve()),
}));

import { onScribeStatus, scribeStatus, type ScribeStatus } from "../ipc/scribe";
import { synthesizeVoice } from "../ipc/voice";
import { DEFAULT_VOICE_SETTINGS, useVoice } from "../store/voice";
import { useSupervision } from "../store/supervision";
import {
  SCRIBE_POLL_MS,
  SCRIBE_EVENT_TTL_MS,
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
  vi.mocked(onScribeStatus).mockReset();
  vi.mocked(onScribeStatus).mockImplementation(() => Promise.resolve(() => {}));
  vi.mocked(synthesizeVoice).mockClear();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    loaded: true,
  });
  useSupervision.setState({ statuses: { session: "working" } });
});

describe("Scribe poll lifecycle", () => {
  it("switches to event-driven updates after the first Scribe event", async () => {
    let emit: ((status: ScribeStatus) => void) | undefined;
    vi.mocked(onScribeStatus).mockImplementation(
      (callback) => {
        emit = callback;
        return Promise.resolve(() => {});
      },
    );
    startScribePoll();
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: { permission: true, question: true, completion: false, failure: false },
    });
    await Promise.resolve();
    expect(scribeStatus).toHaveBeenCalledTimes(1);
    emit?.({ listening: true, generation: 1, observedAtMs: Date.now(), sourceIdentity: "v1" });
    // The event remains authoritative while its age is strictly below the
    // TTL. At the exact boundary the fallback coordinator is expected to
    // resume, so keep this assertion just inside the freshness window.
    await vi.advanceTimersByTimeAsync(SCRIBE_EVENT_TTL_MS - 1);
    expect(scribeStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(scribeStatus).toHaveBeenCalledTimes(2);
  });

  it("ignores malformed, stale, and out-of-order Scribe events", async () => {
    let emit: ((status: ScribeStatus) => void) | undefined;
    vi.mocked(onScribeStatus).mockImplementation((callback) => {
      emit = callback;
      return Promise.resolve(() => {});
    });
    startScribePoll();
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: { permission: true, question: true, completion: false, failure: false },
    });
    await Promise.resolve();
    emit?.({ listening: true });
    emit?.({ listening: true, generation: 2, observedAtMs: Date.now() - SCRIBE_EVENT_TTL_MS - 1, sourceIdentity: "v1" });
    emit?.({ listening: true, generation: 2, observedAtMs: Date.now(), sourceIdentity: "v1" });
    emit?.({ listening: false, generation: 1, observedAtMs: Date.now(), sourceIdentity: "v1" });
    await vi.advanceTimersByTimeAsync(SCRIBE_EVENT_TTL_MS - 1);
    expect(scribeStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(scribeStatus).toHaveBeenCalledTimes(2);
  });

  it("does not call Scribe while voice announcements are disabled", async () => {
    startScribePoll();

    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 4);

    expect(scribeStatus).not.toHaveBeenCalled();
  });

  it("starts one poller when required and stops it when either gate turns off", async () => {
    startScribePoll();
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });
    await Promise.resolve();

    expect(scribeStatus).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 3);
    expect(scribeStatus).toHaveBeenCalledTimes(4);

    useVoice.setState({ volume: 0.4 });
    startScribePoll();
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS);
    expect(scribeStatus).toHaveBeenCalledTimes(5);

    useVoice.setState({
      announceOnAttention: false,
      announcementPolicy: {
        permission: false,
        question: false,
        completion: false,
        failure: false,
      },
    });
    await vi.advanceTimersByTimeAsync(SCRIBE_POLL_MS * 4);
    expect(scribeStatus).toHaveBeenCalledTimes(5);

    useVoice.setState({
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });
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
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });
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
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });

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
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });
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
    useVoice.setState({
      enabled: true,
      announceOnAttention: true,
      announcementPolicy: {
        permission: true,
        question: true,
        completion: false,
        failure: false,
      },
    });
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
