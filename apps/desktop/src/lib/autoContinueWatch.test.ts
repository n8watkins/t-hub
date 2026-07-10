// Auto-continue WATCHER logic — the two safety properties the xhigh review
// required before this could EARN its default-ON:
//
//   1. THE GATE (HIGH-1): the pane-text trigger injects ONLY when supervision
//      INDEPENDENTLY confirms the tile's session is exhausted AND past its reset.
//      A healthy tile that merely DISPLAYS the modal's wording (a test fixture, a
//      PR diff, docs, this very repo) is not exhausted per supervision, so
//      text-as-DATA can never trigger a recovery. Requiring past-reset also closes
//      the dismiss/retry loop (never recover a still-blocked, pre-reset window).
//   2. THE SPLIT WRITE (HIGH-2): recovery sends ESC as its OWN PTY write, then —
//      after a settle delay — the continue text + CR, so ESC is an unambiguous
//      standalone dismiss and a trailing CR can never land on the default PAID
//      option.
import {
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
  vi,
} from "vitest";

// Controllable seams (hoisted so the vi.mock factories can close over them).
const h = vi.hoisted(() => ({
  paneText: {} as Record<string, string>,
  writes: [] as Array<[string, string]>,
}));

vi.mock("./terminalTail", () => ({
  readTerminalTailText: (id: string) => h.paneText[id] ?? "",
}));
vi.mock("../ipc/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc/client")>()),
  writeTerminal: (id: string, data: string) => {
    h.writes.push([id, data]);
    return Promise.resolve();
  },
}));
vi.mock("./notify", () => ({ notify: () => {} }));
vi.mock("./captainAttribution", () => ({ captainSubjectForSession: () => null }));

import {
  scanPanes,
  _resetAutoContinueForTest,
  RECOVERY_ESC_SETTLE_MS,
} from "./autoContinueWatch";
import { ESC, CR } from "./usageLimit";
import { useWorkspace } from "../store/workspace";
import { useSupervision } from "../store/supervision";
import { useAutoContinue } from "../store/autoContinue";
import { useSettings } from "../store/settings";
import type { TerminalInfo } from "../ipc/types";
import type { StatusSnapshot } from "../ipc/model";

const ID = "t1";
const TMUX = "th_t1";
const SESSION = "sess1";

// The real usage-limit modal wording (see usageLimit.test.ts / REPORT.md).
const MODAL = [
  "You've hit your usage limit · resets 3:00pm",
  "❯ Add funds to continue with usage credits",
  "  Switch to Team plan",
  "  Stop and wait for limit to reset",
].join("\n");

// A fixed clock: 1e12 ms => 1e9 s. Reset windows are chosen relative to this.
const NOW_MS = 1_000_000_000_000;
const NOW_S = NOW_MS / 1000;
const PAST_RESET = NOW_S - 100_000; // window reopened long ago
const FUTURE_RESET = NOW_S + 100_000; // window still closed

function mkTerminal(): TerminalInfo {
  return {
    id: ID,
    tmuxSession: TMUX,
    cwd: "/x",
    title: "claude", // clientForTerminal(ID) === "claude"
    state: "running",
  } as unknown as TerminalInfo;
}

/** Seed a supervision snapshot marking the session exhausted with `resetsAt`. */
function seedExhausted(resetsAt: number, usedPercentage = 100): void {
  const snap: StatusSnapshot = {
    sessionId: SESSION,
    rateLimitsPresent: true,
    fiveHour: { usedPercentage, resetsAt },
    ingestedAtMs: NOW_MS,
    tmuxSession: TMUX,
  };
  useSupervision.setState({
    snapshots: { [SESSION]: snap },
    sessionIdByTmux: { [TMUX]: SESSION },
  });
}

const flush = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW_MS);
  h.paneText = {};
  h.writes = [];
  _resetAutoContinueForTest();
  useWorkspace.setState({ terminals: { [ID]: mkTerminal() } });
  useAutoContinue.setState({ optedOut: {} }); // default ON
  useSettings.setState({ autoContinueText: "continue" });
  useSupervision.setState({ snapshots: {}, sessionIdByTmux: {}, statuses: {} });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("scanPanes GATE — text-as-DATA never injects into a healthy tile", () => {
  it("does NOT recover a tile showing the modal when supervision is silent", async () => {
    // The classic false positive: a healthy session merely DISPLAYS the modal
    // wording (no rate_limits data at all). Must never inject.
    h.paneText[ID] = MODAL;
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS + 50);
    expect(h.writes).toEqual([]);
  });

  it("does NOT recover when exhausted but still PRE-reset (closes the loop)", async () => {
    // Genuinely blocked, but the window has not reopened yet — recovering now
    // would just re-hit the limit and re-show the modal every scan.
    h.paneText[ID] = MODAL;
    seedExhausted(FUTURE_RESET);
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS + 50);
    expect(h.writes).toEqual([]);
  });

  it("does NOT recover when near the cap but below the exhausted threshold", async () => {
    h.paneText[ID] = MODAL;
    seedExhausted(PAST_RESET, 95); // 95% < EXHAUSTED_PCT (99)
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS + 50);
    expect(h.writes).toEqual([]);
  });

  it("does NOT recover a tile that is NOT showing the modal, even if exhausted", async () => {
    h.paneText[ID] = "just some ordinary output, all healthy here";
    seedExhausted(PAST_RESET);
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS + 50);
    expect(h.writes).toEqual([]);
  });
});

describe("scanPanes RECOVERY — a genuinely-blocked, past-reset tile DOES recover", () => {
  it("injects, and does so as a SPLIT write: ESC alone, THEN text+CR after the settle", async () => {
    h.paneText[ID] = MODAL;
    seedExhausted(PAST_RESET);

    scanPanes();
    await flush();
    // Before the settle delay elapses, ONLY the standalone ESC has been sent.
    expect(h.writes).toEqual([[ID, ESC]]);

    // After the settle, the continue text + Enter is sent as a SECOND write.
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS);
    expect(h.writes).toEqual([
      [ID, ESC],
      [ID, "continue" + CR],
    ]);
    // The ESC never rides with the text, and the submit carries exactly one CR.
    expect(h.writes[1][1].startsWith(ESC)).toBe(false);
    expect(h.writes[1][1].split(CR)).toHaveLength(2);
  });

  it("recovers a reset window exactly ONCE across repeated scans (no re-fire loop)", async () => {
    h.paneText[ID] = MODAL;
    seedExhausted(PAST_RESET);

    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS);
    expect(h.writes).toHaveLength(2); // ESC + text once

    // Modal still on screen next scan (flapping / still-rendering) — same window,
    // so the shared `handled` dedup must suppress a second injection.
    scanPanes();
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS);
    expect(h.writes).toHaveLength(2); // unchanged — recovered once
  });

  it("never injects into an OPTED-OUT tile even when genuinely blocked", async () => {
    h.paneText[ID] = MODAL;
    seedExhausted(PAST_RESET);
    useAutoContinue.setState({ optedOut: { [ID]: true } });
    scanPanes();
    await vi.advanceTimersByTimeAsync(RECOVERY_ESC_SETTLE_MS + 50);
    expect(h.writes).toEqual([]);
  });
});
