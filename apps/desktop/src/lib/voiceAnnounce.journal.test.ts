import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  claimVoiceAnnouncement: vi.fn(() =>
    Promise.resolve({ shouldAnnounce: true }),
  ),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
  recordVoiceAnnouncementOutcome: vi.fn(() => Promise.resolve()),
  recoverVoiceAnnouncements: vi.fn(() => Promise.resolve(null)),
  seedVoiceAnnouncementBoundary: vi.fn(() => Promise.resolve()),
}));
vi.mock("../ipc/client05", () => ({
  onJournal: vi.fn(),
}));
vi.mock("./voiceAudio", () => {
  class VoiceAudioError extends Error {
    constructor(
      public readonly kind: "playback" | "device",
      message: string,
    ) {
      super(message);
    }
  }
  return {
    VoiceAudioError,
    playWavBase64: vi.fn(() => Promise.resolve()),
  };
});
vi.mock("./notify", () => ({
  notify: vi.fn(),
}));

import {
  claimVoiceAnnouncement,
  recordVoiceAnnouncementOutcome,
  seedVoiceAnnouncementBoundary,
  synthesizeVoice,
  type VoiceAnnouncementKind,
} from "../ipc/voice";
import { onJournal } from "../ipc/client05";
import type { JournalEvent, JournalEventType } from "../ipc/protocol";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";
import { useEngineRuntime } from "../store/engineRuntime";
import { useSupervision } from "../store/supervision";
import { useWorkspace } from "../store/workspace";
import { playWavBase64, VoiceAudioError } from "./voiceAudio";
import {
  _pendingTextForTest,
  _resetVoiceAnnounceForTest,
  _setScribeListeningForTest,
  flushPending,
  handleJournalEvent,
  mountVoiceAnnounce,
} from "./voiceAnnounce";

function journal(
  seq: number,
  eventType: JournalEventType,
  payload: unknown = {},
  authorityKind: VoiceAnnouncementKind | null = defaultAuthority(eventType),
): JournalEvent {
  return {
    replayed: false,
    entry: {
      seq,
      timestamp_ms: 1,
      source: "hook",
      event_id: `provider-event:v1:${seq}`,
      entity_id: "session-1",
      event_type: eventType,
      payload,
    },
    ...(authorityKind
      ? {
          voice_announcement: {
            kind: authorityKind,
            sessionId: "session-1",
            status:
              authorityKind === "failure"
                ? ("failed" as const)
                : authorityKind === "completion"
                  ? ("completed" as const)
                  : authorityKind === "permission"
                    ? ("needsPermission" as const)
                    : ("needsQuestion" as const),
          },
        }
      : {}),
  };
}

function defaultAuthority(
  eventType: JournalEventType,
): VoiceAnnouncementKind | null {
  const authorityByEvent: Partial<
    Record<JournalEventType, VoiceAnnouncementKind>
  > = {
    permissionRequest: "permission",
    elicitation: "question",
    stop: "completion",
    stopFailure: "failure",
  };
  return authorityByEvent[eventType] ?? null;
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
    shouldAnnounce: true,
  });
  vi.mocked(synthesizeVoice).mockResolvedValue("d2F2");
  vi.mocked(playWavBase64).mockResolvedValue();
  _resetVoiceAnnounceForTest();
  _setScribeListeningForTest(false);
  useEngineRuntime.setState({ status: null });
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    enabled: true,
    announcementPolicy: {
      permission: true,
      question: true,
      completion: true,
      failure: true,
    },
    deliveryFailure: null,
  });
  useSupervision.setState({
    trees: {},
    statuses: {},
    snapshots: {},
    sessionIdByTmux: { th_term1: "session-1" },
  });
  useWorkspace.setState({
    tabs: [],
    terminals: {
      term1: {
        id: "term1",
        tmuxSession: "th_term1",
        cwd: "/work/safe-project",
        title: "SECRET typed prompt",
        state: "live",
      },
    },
    userLabels: { term1: "Safe captain" },
    labels: { term1: "SECRET typed prompt" },
  });
});

describe("provider-neutral journal announcements", () => {
  const cases: Array<
    [JournalEventType, VoiceAnnouncementKind, string]
  > = [
    ["permissionRequest", "permission", "needs permission"],
    ["elicitation", "question", "has a question"],
    ["stop", "completion", "completed"],
    ["stopFailure", "failure", "failed"],
  ];

  for (const [eventType, kind, phrase] of cases) {
    it(`maps ${eventType} to the ${kind} policy`, async () => {
      if (eventType === "stop") {
        useSupervision.setState({ statuses: { "session-1": "completed" } });
      }
      await handleJournalEvent(
        journal(10, eventType, {
          prompt: "SECRET provider content",
          message: "SECRET failure detail",
        }),
        10_000,
      );
      await settle();

      expect(claimVoiceAnnouncement).toHaveBeenCalledWith(
        10,
        kind,
        "provider-event:v1:10",
      );
      const text = vi.mocked(synthesizeVoice).mock.calls[0][0];
      expect(text).toContain("Safe captain");
      expect(text).toContain(phrase);
      expect(text).not.toContain("SECRET");
    });
  }

  it("does not synthesize when the durable claim rejects replay or policy", async () => {
    vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
      shouldAnnounce: false,
    });
    await handleJournalEvent(journal(11, "permissionRequest"), 10_000);
    await settle();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("records synthesis failures explicitly", async () => {
    vi.mocked(synthesizeVoice).mockRejectedValue(new Error("server refused"));
    await handleJournalEvent(journal(12, "stopFailure"), 10_000);
    await settle();
    expect(useVoice.getState().deliveryFailure).toMatchObject({
      kind: "synthesis",
      detail: "server refused",
    });
  });

  it("records audio device failures explicitly", async () => {
    useSupervision.setState({ statuses: { "session-1": "completed" } });
    vi.mocked(playWavBase64).mockRejectedValue(
      new VoiceAudioError("device", "no output device"),
    );
    await handleJournalEvent(journal(13, "stop"), 10_000);
    await settle();
    expect(useVoice.getState().deliveryFailure).toMatchObject({
      kind: "device",
      detail: "no output device",
    });
  });

  it("observes Stop without announcing while the reducer is waiting on subagents", async () => {
    useSupervision.setState({
      statuses: { "session-1": "waitingOnSubagents" },
    });
    await handleJournalEvent(journal(14, "stop", {}, null), 10_000);
    await settle();
    expect(claimVoiceAnnouncement).not.toHaveBeenCalled();
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("maps clean and abnormal SessionEnd from reducer authority", async () => {
    useSupervision.setState({ statuses: { "session-1": "completed" } });
    await handleJournalEvent(
      journal(15, "sessionEnd", {}, "completion"),
      10_000,
    );
    await settle();
    expect(claimVoiceAnnouncement).toHaveBeenLastCalledWith(
      15,
      "completion",
      "provider-event:v1:15",
    );

    _resetVoiceAnnounceForTest();
    _setScribeListeningForTest(false);
    useSupervision.setState({ statuses: { "session-1": "failed" } });
    await handleJournalEvent(journal(16, "sessionEnd", {}, "failure"), 20_000);
    await settle();
    expect(claimVoiceAnnouncement).toHaveBeenLastCalledWith(
      16,
      "failure",
      "provider-event:v1:16",
    );
  });

  it("never overwrites a queued failure with a lower-severity completion", async () => {
    _setScribeListeningForTest(true);
    await handleJournalEvent(journal(17, "stopFailure"), 10_000);
    useSupervision.setState({ statuses: { "session-1": "completed" } });
    await handleJournalEvent(journal(18, "stop"), 10_001);
    expect(_pendingTextForTest()).toContain("failed");
    expect(_pendingTextForTest()).not.toContain("completed");
  });

  it("announces completion authority carried by a child drain event", async () => {
    await handleJournalEvent(
      journal(19, "subagentStop", {}, "completion"),
      30_000,
    );
    await settle();
    expect(claimVoiceAnnouncement).toHaveBeenCalledWith(
      19,
      "completion",
      "provider-event:v1:19",
    );
    expect(synthesizeVoice).toHaveBeenCalledWith(
      expect.stringContaining("completed"),
      expect.any(String),
      expect.any(String),
    );
  });

  it("interrupts claimed authority for a provider session outside the visible crew", async () => {
    vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
      shouldAnnounce: true,
      attemptId: "voice-attempt:v1:20",
    });
    useSupervision.setState({ sessionIdByTmux: {} });

    await handleJournalEvent(journal(20, "permissionRequest"), 30_000);
    await settle();

    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(recordVoiceAnnouncementOutcome).toHaveBeenCalledWith(
      "voice-attempt:v1:20",
      "interrupted",
      expect.stringContaining("active Captain, Crew, or Assignment"),
    );
  });

  it("records a held input cue as interrupted when the request resolves", async () => {
    vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
      shouldAnnounce: true,
      attemptId: "voice-attempt:v1:21",
    });
    _setScribeListeningForTest(true);
    useSupervision.setState({
      statuses: { "session-1": "needsPermission" },
    });
    await handleJournalEvent(journal(21, "permissionRequest"), 30_000);
    expect(_pendingTextForTest()).toContain("needs permission");

    useSupervision.setState({ statuses: { "session-1": "working" } });
    await flushPending(31_000);

    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(recordVoiceAnnouncementOutcome).toHaveBeenCalledWith(
      "voice-attempt:v1:21",
      "interrupted",
      expect.stringContaining("resolved before"),
    );
    expect(_pendingTextForTest()).toBeNull();
  });

  it("resolves a held cue against its exact authority session", async () => {
    vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
      shouldAnnounce: true,
      attemptId: "voice-attempt:v1:22",
    });
    _setScribeListeningForTest(true);
    useSupervision.setState({
      statuses: {
        "session-1": "needsPermission",
        "session-2": "needsQuestion",
      },
    });
    await handleJournalEvent(journal(22, "permissionRequest"), 30_000);

    useSupervision.setState({
      statuses: {
        "session-1": "working",
        "session-2": "needsQuestion",
      },
    });
    await flushPending(31_000);

    expect(recordVoiceAnnouncementOutcome).toHaveBeenCalledWith(
      "voice-attempt:v1:22",
      "interrupted",
      expect.any(String),
    );
    expect(synthesizeVoice).not.toHaveBeenCalled();
  });

  it("retains a resolved cue until its interrupted outcome is durable", async () => {
    vi.mocked(claimVoiceAnnouncement).mockResolvedValue({
      shouldAnnounce: true,
      attemptId: "voice-attempt:v1:23",
    });
    vi.mocked(recordVoiceAnnouncementOutcome)
      .mockRejectedValueOnce(new Error("disk unavailable"))
      .mockResolvedValueOnce();
    _setScribeListeningForTest(true);
    useSupervision.setState({
      statuses: { "session-1": "needsPermission" },
    });
    await handleJournalEvent(journal(23, "permissionRequest"), 30_000);
    useSupervision.setState({ statuses: { "session-1": "working" } });

    flushPending(31_000);
    await settle();
    expect(_pendingTextForTest()).not.toBeNull();
    expect(useVoice.getState().deliveryFailure).toMatchObject({
      kind: "interrupted",
      detail: expect.stringContaining("disk unavailable"),
    });

    await flushPending(32_000);
    expect(recordVoiceAnnouncementOutcome).toHaveBeenCalledTimes(2);
    expect(_pendingTextForTest()).toBeNull();
    expect(useVoice.getState().deliveryFailure).toBeNull();
  });

  it("seeds replay-marked events and admits the next live event", async () => {
    let listener: ((event: JournalEvent) => void) | undefined;
    vi.mocked(onJournal).mockImplementation((callback) => {
      listener = callback;
      return Promise.resolve(() => {});
    });

    mountVoiceAnnounce();
    await settle();

    listener?.({ ...journal(50, "permissionRequest"), replayed: true });
    listener?.(journal(51, "permissionRequest"));
    await settle();
    await settle();

    expect(seedVoiceAnnouncementBoundary).toHaveBeenCalledWith(50);
    expect(claimVoiceAnnouncement).not.toHaveBeenCalledWith(
      50,
      expect.anything(),
      expect.anything(),
    );
    expect(claimVoiceAnnouncement).toHaveBeenCalledWith(
      51,
      "permission",
      "provider-event:v1:51",
    );
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
  });
});
