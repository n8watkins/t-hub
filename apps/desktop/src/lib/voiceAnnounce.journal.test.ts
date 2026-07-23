import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/voice", () => ({
  claimVoiceAnnouncement: vi.fn(() =>
    Promise.resolve({ shouldAnnounce: true }),
  ),
  synthesizeVoice: vi.fn(() => Promise.resolve("d2F2")),
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
  synthesizeVoice,
  type VoiceAnnouncementKind,
} from "../ipc/voice";
import type { JournalEvent, JournalEventType } from "../ipc/protocol";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";
import { useEngineRuntime } from "../store/engineRuntime";
import { useSupervision } from "../store/supervision";
import { useWorkspace } from "../store/workspace";
import { playWavBase64, VoiceAudioError } from "./voiceAudio";
import {
  _resetVoiceAnnounceForTest,
  _setScribeListeningForTest,
  handleJournalEvent,
} from "./voiceAnnounce";

function journal(
  seq: number,
  eventType: JournalEventType,
  payload: unknown = {},
): JournalEvent {
  return {
    entry: {
      seq,
      timestamp_ms: 1,
      source: "hook",
      entity_id: "session-1",
      event_type: eventType,
      payload,
    },
  };
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
      await handleJournalEvent(
        journal(10, eventType, {
          prompt: "SECRET provider content",
          message: "SECRET failure detail",
        }),
        10_000,
      );
      await settle();

      expect(claimVoiceAnnouncement).toHaveBeenCalledWith(10, kind);
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
});
