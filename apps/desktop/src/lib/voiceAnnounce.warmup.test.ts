// Startup-warmup coverage for the voice announcer, in its OWN file: the
// warmup window latches OFF the first time inWarmup() runs before start()
// (deadline 0), so this must be the module's FIRST interaction - it cannot
// share an instance with voiceAnnounce.test.ts, whose gating cases exercise
// the post-warmup path. Production matches this file's ordering: mount starts
// warmup before the store subscription that feeds handleStatusesChange exists.
//
// The scenario pinned here is the review MAJOR: on the first connect the
// agent replays its journal, so a session that was ALREADY blocked before
// launch re-emits needsPermission - pre-fix that spoke a stale announcement
// at every launch (twice, when the replay spanned the debounce window).
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
import {
  handleStatusesChange,
  _startVoiceAnnounceWarmupForTest,
  ANNOUNCE_MIN_GAP_MS,
} from "./voiceAnnounce";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";
import { useSupervision } from "../store/supervision";
import type { SessionStatus } from "../ipc/model";

function statuses(
  map: Record<string, SessionStatus>,
): Record<string, SessionStatus> {
  return { ...map };
}

beforeEach(() => {
  vi.mocked(synthesizeVoice).mockClear();
  useVoice.setState({
    ...DEFAULT_VOICE_SETTINGS,
    enabled: true,
    announceOnAttention: true,
    voice: "v.onnx",
    loaded: true,
  });
  useSupervision.setState({
    trees: {},
    statuses: {},
    snapshots: {},
    sessionIdByTmux: {},
  });
});

describe("startup warmup (journal replay)", () => {
  it("swallows the replay burst, seeds the baseline, then speaks on real transitions", async () => {
    // Fake timers drive the warmup's absolute deadline (it reads Date.now).
    vi.useFakeTimers();
    try {
      _startVoiceAnnounceWarmupForTest();

      // The startup replay: an ALREADY-BLOCKED session re-emits its status.
      // It must stay silent...
      handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
      await Promise.resolve();
      expect(synthesizeVoice).not.toHaveBeenCalled();

      // ...through a SLOW burst that keeps flowing (sub-grace gaps re-arm the
      // 1.5s quiet window) and spans the 5s announce debounce - the "speaks
      // twice" half of the reported bug. Total elapsed stays under the 6s
      // absolute warmup cap.
      let t = 0;
      for (let i = 0; i < 4; i++) {
        vi.advanceTimersByTime(1200);
        t += 1200;
        handleStatusesChange(statuses({ "sess-1": "needsPermission" }), t);
      }
      vi.advanceTimersByTime(300);
      t += 300; // t = 5100 > ANNOUNCE_MIN_GAP_MS, still < the 6000ms cap
      expect(t).toBeGreaterThan(ANNOUNCE_MIN_GAP_MS);
      handleStatusesChange(
        statuses({ "sess-1": "needsPermission", "sess-2": "needsQuestion" }),
        t,
      );
      await Promise.resolve();
      expect(synthesizeVoice).not.toHaveBeenCalled();

      // Warmup over (absolute cap). The replayed sessions are SEEDED in the
      // baseline: re-delivering the same statuses is not a transition.
      vi.advanceTimersByTime(10_000);
      const after = 20_000;
      handleStatusesChange(
        statuses({ "sess-1": "needsPermission", "sess-2": "needsQuestion" }),
        after,
      );
      await Promise.resolve();
      expect(synthesizeVoice).not.toHaveBeenCalled();

      // A REAL post-startup transition speaks (no terminal mapping seeded, so
      // the generic label is used - the announcement itself is the assertion).
      handleStatusesChange(
        statuses({
          "sess-1": "needsPermission",
          "sess-2": "needsQuestion",
          "sess-3": "needsPermission",
        }),
        after + 100,
      );
      await Promise.resolve();
      await Promise.resolve();
      expect(synthesizeVoice).toHaveBeenCalledTimes(1);
      expect(vi.mocked(synthesizeVoice).mock.calls[0][0]).toBe(
        "A session needs your attention",
      );
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });
});
