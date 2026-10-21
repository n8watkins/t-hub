import { beforeEach, afterEach, expect, it, vi } from "vitest";
const h = vi.hoisted(() => ({ text: "", notify: vi.fn(), write: vi.fn() }));
vi.mock("./terminalTail", () => ({ readTerminalTailText: () => h.text }));
vi.mock("./notify", () => ({ notify: h.notify }));
vi.mock("./captainAttribution", () => ({ captainSubjectForSession: () => null }));
vi.mock("../ipc/client", async (original) => ({
  ...(await original<typeof import("../ipc/client")>()), deliverAgentInput: h.write,
}));
import { scanPanes, exhaustedReset, _resetAutoContinueForTest } from "./autoContinueWatch";
import { useWorkspace } from "../store/workspace";
import { useSupervision } from "../store/supervision";
import { useAutoContinue } from "../store/autoContinue";
import type { TerminalInfo } from "../ipc/types";
import type { StatusSnapshot } from "../ipc/model";
const NOW = 1000000000000;
const snapshot = (): StatusSnapshot => ({ sessionId: "s1", ingestedAtMs: NOW,
  rateLimitsPresent: true, fiveHour: { usedPercentage: 100, resetsAt: NOW / 1000 - 10 } });
beforeEach(() => {
  vi.useFakeTimers(); vi.setSystemTime(NOW); vi.clearAllMocks(); _resetAutoContinueForTest();
  h.text = "You've hit your usage limit · resets 3:00pm\n❯ Add funds to continue with usage credits\nSwitch to Team plan\nStop and wait for limit to reset";
  useWorkspace.setState({ terminals: { t1: { id: "t1", title: "claude", tmuxSession: "th_t1", cwd: "/tmp" } as TerminalInfo } });
  useAutoContinue.setState({ optedOut: {} });
  useSupervision.setState({ snapshots: { s1: snapshot() }, sessionIdByTmux: { th_t1: "s1" }, statuses: {} });
});
afterEach(() => { vi.useRealTimers(); });
it("alerts once and never writes even for a matching blocked terminal", () => {
  scanPanes(); scanPanes(); expect(h.notify).toHaveBeenCalledTimes(1); expect(h.write).not.toHaveBeenCalled();
  expect(h.notify.mock.calls[0][2]).toContain("resume manually");
});
it("waits for every exhausted window and refuses an unknown reset", () => {
  const s = snapshot(); s.sevenDay = { usedPercentage: 100, resetsAt: NOW / 1000 + 60 };
  expect(exhaustedReset(s)).toBe(NOW / 1000 + 60);
  s.sevenDay.resetsAt = undefined; expect(exhaustedReset(s)).toBeNull();
  s.sevenDay.resetsAt = NaN; expect(exhaustedReset(s)).toBeNull();
});
it.each(["stale", "future", "no-modal", "opt-out", "codex"])("does not alert for %s evidence", (kind) => {
  if (kind === "stale") useSupervision.setState({ snapshots: { s1: { ...snapshot(), ingestedAtMs: NOW - 180001 } } });
  if (kind === "future") useSupervision.setState({ snapshots: { s1: { ...snapshot(), ingestedAtMs: NOW + 1 } } });
  if (kind === "no-modal") h.text = "ordinary output";
  if (kind === "opt-out") useAutoContinue.setState({ optedOut: { t1: true } });
  if (kind === "codex") useWorkspace.setState({ terminals: { t1: { ...useWorkspace.getState().terminals.t1, title: "codex" } } });
  scanPanes(); expect(h.notify).not.toHaveBeenCalled(); expect(h.write).not.toHaveBeenCalled();
});
