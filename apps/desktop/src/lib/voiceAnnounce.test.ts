// Announce-on-attention gating tests: OFF by default (opt-in), never speaks
// while the master enable is off, speaks exactly once per transition INTO a
// needs-input state, a needs-input-to-needs-input flip stays quiet, bursts
// debounce to one cue per window (charged only on synthesis SUCCESS), the
// startup warmup swallows the journal-replay burst while seeding the
// baseline, and transitions that happened while the feature was off never
// replay when it turns on.
//
// Outside warmup, a session APPEARING already-blocked (undefined ->
// needs-input) is a real transition - a crew spawned mid-flight that
// immediately hit a permission prompt should speak. The startup case where
// that same shape is a stale replay is exactly what the warmup window covers.
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
vi.mock("./notify", () => ({
  notify: vi.fn(),
}));

import { synthesizeVoice } from "../ipc/voice";
import { playWavBase64 } from "./voiceAudio";
import { notify } from "./notify";
import {
  handleStatusesChange,
  applyScribeListening,
  flushPending,
  _resetVoiceAnnounceForTest,
  _setScribeListeningForTest,
  _pendingTextForTest,
  ANNOUNCE_MIN_GAP_MS,
  SCRIBE_TAIL_MS,
  FALLBACK_ALERT_MIN_GAP_MS,
} from "./voiceAnnounce";
import { useVoice, DEFAULT_VOICE_SETTINGS } from "../store/voice";
import { useEngineRuntime } from "../store/engineRuntime";
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
  vi.mocked(synthesizeVoice).mockResolvedValue("d2F2");
  vi.mocked(playWavBase64).mockClear();
  vi.mocked(notify).mockClear();
  // Default unmanaged so the #52 chime path is exercised; the F6 case sets this.
  useEngineRuntime.setState({ status: null });
  _resetVoiceAnnounceForTest();
  _setScribeListeningForTest(false);
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
    tabs: [],
    terminals: {
      cap00001: {
        id: "cap00001",
        tmuxSession: "th_cap00001",
        cwd: "/tmp/ship",
        // A volatile Claude title reflecting the user's typed input - the
        // spoken name must NEVER be this (it must be the stable rename below).
        title: "please fix the bug",
        state: "live",
      },
    },
    // The stable identity the announcement should speak (a persisted rename).
    userLabels: { cap00001: "captain" },
    labels: { cap00001: "captain" },
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

  it("a FAILED synthesis does not eat the debounce window", async () => {
    vi.mocked(synthesizeVoice).mockRejectedValueOnce(new Error("server down"));
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    expect(playWavBase64).not.toHaveBeenCalled();
    // Well inside the 5s window: because the failure never charged
    // lastSpokenAt, the next transition still speaks.
    handleStatusesChange(statuses({ "sess-1": "working" }), 500);
    handleStatusesChange(statuses({ "sess-1": "needsQuestion" }), 1000);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(2);
    expect(playWavBase64).toHaveBeenCalledTimes(1);
  });
});

describe("spoken label is the STABLE identity, never the typed input", () => {
  it("speaks the user rename, not the volatile Claude title", async () => {
    // beforeEach seeds title 'please fix the bug' (the typed input) + rename
    // 'captain'. The announcement must speak the rename.
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    const spoken = vi.mocked(synthesizeVoice).mock.calls[0][0];
    expect(spoken).toContain("captain");
    expect(spoken).not.toContain("please fix the bug");
  });

  it("falls back to the workspace TAB NAME (not the title) when there is no rename", async () => {
    useWorkspace.setState({
      tabs: [{ id: "t1", name: "Flagship", order: ["cap00001"] }],
      terminals: {
        cap00001: {
          id: "cap00001",
          tmuxSession: "th_cap00001",
          cwd: "/tmp/ship",
          title: "summarize this transcript",
          state: "live",
        },
      },
      userLabels: {},
      // The merged `labels` still carries the volatile title - proof we do NOT
      // read it (we read userLabels + the tab name instead).
      labels: { cap00001: "summarize this transcript" },
    });
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    const spoken = vi.mocked(synthesizeVoice).mock.calls[0][0];
    expect(spoken).toContain("Flagship");
    expect(spoken).not.toContain("summarize this transcript");
  });

  it("falls back to the cwd basename when there is no rename or tab", async () => {
    useWorkspace.setState({
      tabs: [],
      terminals: {
        cap00001: {
          id: "cap00001",
          tmuxSession: "th_cap00001",
          cwd: "/home/n/wt-feature/webapp",
          title: "typed input here",
          state: "live",
        },
      },
      userLabels: {},
      labels: {},
    });
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    const spoken = vi.mocked(synthesizeVoice).mock.calls[0][0];
    expect(spoken).toContain("webapp");
    expect(spoken).not.toContain("typed input here");
  });
});

describe("Scribe voice-gate (hold while dictating, deliver when stopped)", () => {
  /** Seed the store statuses so flushPending's "still blocked?" re-scan sees
   *  them, and mirror them into handleStatusesChange for transition detection. */
  function seedStoreStatuses(map: Record<string, SessionStatus>): void {
    useSupervision.setState({ statuses: { ...map } });
  }

  it("HOLDS the cue while the general is dictating (does not speak)", async () => {
    _setScribeListeningForTest(true);
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(_pendingTextForTest()).toMatch(/needs your attention$/);
    expect(_pendingTextForTest()).toContain("captain");
  });

  it("DELIVERS the held cue when dictation stops, if something is still blocked", async () => {
    _setScribeListeningForTest(true);
    seedStoreStatuses({ "sess-1": "needsPermission" });
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
    // The general stops: the tail flush finds sess-1 still blocked -> speak.
    flushPending(1000);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    expect(vi.mocked(synthesizeVoice).mock.calls[0][0]).toContain("captain");
    expect(_pendingTextForTest()).toBeNull();
  });

  it("DROPS the held cue silently if the situation resolved during dictation", async () => {
    _setScribeListeningForTest(true);
    seedStoreStatuses({ "sess-1": "needsPermission" });
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(_pendingTextForTest()).not.toBeNull();
    // While they talked, the captain unblocked (no longer needs input).
    seedStoreStatuses({ "sess-1": "working" });
    flushPending(1000);
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
    expect(_pendingTextForTest()).toBeNull();
  });

  it("COALESCES multiple held transitions into one (latest wins, no backlog)", async () => {
    // Two sessions map to two terminals so their (stable) labels differ.
    useSupervision.setState({
      statuses: {},
      sessionIdByTmux: { th_cap00001: "sess-1", th_cap00002: "sess-2" },
    });
    useWorkspace.setState({
      tabs: [],
      terminals: {
        cap00001: { id: "cap00001", tmuxSession: "th_cap00001", cwd: "/tmp/a", title: "typed a", state: "live" },
        cap00002: { id: "cap00002", tmuxSession: "th_cap00002", cwd: "/tmp/b", title: "typed b", state: "live" },
      },
      userLabels: { cap00001: "captain", cap00002: "crewmate" },
      labels: { cap00001: "captain", cap00002: "crewmate" },
    });
    _setScribeListeningForTest(true);
    // A blocks, then B blocks (a later, separate transition).
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission", "sess-2": "needsQuestion" }),
      100,
    );
    await flush();
    expect(synthesizeVoice).not.toHaveBeenCalled();
    // Only the LATEST is held.
    expect(_pendingTextForTest()).toContain("crewmate");
    // Deliver: both still blocked -> exactly ONE cue (the latest).
    useSupervision.setState({
      statuses: { "sess-1": "needsPermission", "sess-2": "needsQuestion" },
    });
    flushPending(1000);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    expect(vi.mocked(synthesizeVoice).mock.calls[0][0]).toContain("crewmate");
  });

  it("the true->false falling edge arms the tail flush (injected clock)", async () => {
    vi.useFakeTimers();
    try {
      _setScribeListeningForTest(true);
      seedStoreStatuses({ "sess-1": "needsPermission" });
      handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
      // The general stops talking: applyScribeListening(false) arms the tail.
      applyScribeListening(false, 0);
      expect(synthesizeVoice).not.toHaveBeenCalled(); // not yet - within the tail
      vi.advanceTimersByTime(SCRIBE_TAIL_MS);
      await Promise.resolve();
      await Promise.resolve();
      expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it("a resume within the tail cancels delivery (keeps holding)", async () => {
    vi.useFakeTimers();
    try {
      _setScribeListeningForTest(true);
      seedStoreStatuses({ "sess-1": "needsPermission" });
      handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
      applyScribeListening(false, 0); // stopped: arms the tail
      applyScribeListening(true, 100); // resumed before the tail: cancels it
      vi.advanceTimersByTime(SCRIBE_TAIL_MS * 2);
      await Promise.resolve();
      expect(synthesizeVoice).not.toHaveBeenCalled();
      expect(_pendingTextForTest()).not.toBeNull(); // still held
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it("speaks immediately (no hold) when the general is NOT dictating", async () => {
    _setScribeListeningForTest(false);
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(synthesizeVoice).toHaveBeenCalledTimes(1);
    expect(_pendingTextForTest()).toBeNull();
  });

  it("re-arms rather than dropping the held cue if a synthesis is in flight at flush time", async () => {
    vi.useFakeTimers();
    try {
      // The first synthesis HANGS (stays in flight); later ones resolve.
      let releaseFirst: () => void = () => {};
      const firstPending = new Promise<string>((res) => {
        releaseFirst = () => res("d2F2");
      });
      vi.mocked(synthesizeVoice).mockReturnValueOnce(firstPending);

      // Not dictating: a normal cue for sess-1 starts and hangs.
      _setScribeListeningForTest(false);
      useSupervision.setState({ statuses: { "sess-1": "needsPermission" } });
      handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
      expect(synthesizeVoice).toHaveBeenCalledTimes(1);

      // Now the general dictates: a fresh sess-2 transition is HELD.
      _setScribeListeningForTest(true);
      useSupervision.setState({
        statuses: { "sess-1": "needsPermission", "sess-2": "needsQuestion" },
      });
      handleStatusesChange(
        statuses({ "sess-1": "needsPermission", "sess-2": "needsQuestion" }),
        1,
      );
      expect(_pendingTextForTest()).not.toBeNull();

      // Flush WHILE the first synth is still in flight -> the held cue must not
      // be lost: pending kept, no new synth started.
      flushPending(2);
      expect(_pendingTextForTest()).not.toBeNull();
      expect(synthesizeVoice).toHaveBeenCalledTimes(1);

      // The first synth resolves (frees the in-flight slot: its .finally clears
      // `speaking`); the re-armed tail then delivers the held cue.
      releaseFirst();
      await flush(); // 3 microtask ticks: let .then/.catch/.finally settle
      vi.advanceTimersByTime(SCRIBE_TAIL_MS);
      expect(synthesizeVoice).toHaveBeenCalledTimes(2);
      expect(_pendingTextForTest()).toBeNull();
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });
});

describe("never-silent fallback alert (engine unreachable)", () => {
  it("fires a notify('error') when synthesis fails - the dropped cue is not silent", async () => {
    vi.mocked(synthesizeVoice).mockRejectedValue(new Error("connection refused"));
    useVoice.setState({ engine: "kokoro" });

    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();

    // No audio played (the server is down), but the failure surfaces as the
    // error chime + toast instead of vanishing - the whole point of the fix.
    expect(playWavBase64).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledTimes(1);
    const [kind, title, body] = vi.mocked(notify).mock.calls[0];
    expect(kind).toBe("error");
    expect(title).toMatch(/unreachable/i);
    expect(body).toMatch(/kokoro/); // names the engine that failed
  });

  it("debounces repeated failures to one alert per window, then reopens", async () => {
    vi.mocked(synthesizeVoice).mockRejectedValue(new Error("down"));

    // First failure alerts.
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(notify).toHaveBeenCalledTimes(1);

    // A fresh transition inside the window fails again but must NOT re-alert
    // (a persistently-down engine shouldn't chime on every attempt).
    handleStatusesChange(statuses({ "sess-1": "working" }), 100);
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 6000);
    await flush();
    expect(notify).toHaveBeenCalledTimes(1);

    // Past the window, the next failure alerts again.
    handleStatusesChange(statuses({ "sess-1": "working" }), FALLBACK_ALERT_MIN_GAP_MS + 100);
    handleStatusesChange(
      statuses({ "sess-1": "needsPermission" }),
      FALLBACK_ALERT_MIN_GAP_MS + 200,
    );
    await flush();
    expect(notify).toHaveBeenCalledTimes(2);
  });

  it("does NOT fire when synthesis succeeds (no false alarm)", async () => {
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(playWavBase64).toHaveBeenCalledTimes(1);
    expect(notify).not.toHaveBeenCalled();
  });

  it("F6: suppresses the #52 chime when the managed lifecycle owns the fallback", async () => {
    // Managed + fallen back: the supervisor fires its own "Voice fell back"
    // toast, so the announce path must NOT double-chime on a failed synthesis.
    vi.mocked(synthesizeVoice).mockRejectedValue(new Error("down"));
    useEngineRuntime.setState({
      status: {
        managed: true,
        selectedEngine: "kokoro",
        activeEngine: "piper",
        degraded: true,
        level: "amber",
        kokoro: "down",
        piper: "up",
      },
    });
    handleStatusesChange(statuses({ "sess-1": "needsPermission" }), 0);
    await flush();
    expect(notify).not.toHaveBeenCalled();
  });
});

// The startup-warmup (journal replay) coverage lives in its own file,
// voiceAnnounce.warmup.test.ts: the warmup window latches OFF the first time
// inWarmup() runs before start() (deadline 0), so a test that starts warmup
// cannot share a module instance with these gating tests, which exercise the
// post-warmup path first. Production is safe by construction - mount starts
// warmup before the store subscription that feeds handleStatusesChange exists.
