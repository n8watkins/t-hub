// comms-plane Phase 1 (PR #55, MED-1): PIN the rules-engine funnel. The rules
// engine's terminal-writing actions (`sendText`/`run`) are AUTOMATION input, so
// Phase 1 routed them off the human `writeTerminal` path onto the plane's
// `deliverAgentInput`. Without this test, reverting either action back to
// `writeTerminal` would reopen substrate (b) for the rules engine and fail ZERO
// tests - the exact silent-second-writer regression the plane exists to prevent.
// These assertions FAIL on such a bypass (they require plane delivery with the
// `rules-engine` source AND that the human path is never used).

import { describe, it, expect, beforeEach, vi } from "vitest";

// Controllable seams (hoisted so the vi.mock factories can close over them).
const h = vi.hoisted(() => ({
  delivered: [] as Array<[string, string, string]>, // [id, data, source]
  humanWrites: [] as Array<[string, string]>, // [id, data]
}));

vi.mock("../ipc/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc/client")>()),
  deliverAgentInput: (id: string, data: string, source: string) => {
    h.delivered.push([id, data, source]);
    return Promise.resolve();
  },
  writeTerminal: (id: string, data: string) => {
    h.humanWrites.push([id, data]);
    return Promise.resolve();
  },
  // Non-write actions used by other rule kinds - stubbed so importing the module
  // (which installs the engine) touches no real Tauri surface.
  spawnTerminal: () => Promise.resolve({}),
  killTerminal: () => Promise.resolve(),
}));
// The install side-effect subscribes to these; reject so the engine stays idle
// (there is no Tauri event runtime under jsdom). We drive runAction directly.
vi.mock("../ipc/client05", () => ({
  onSessionStatus: () => Promise.reject(new Error("no runtime")),
  onSupervision: () => Promise.reject(new Error("no runtime")),
}));

import { _runRuleActionForTest } from "./rulesMount";
import { useSupervision } from "../store/supervision";
import type { Rule, RuleAction } from "../store/rules";

const SESSION = "sess1";
const ID = "t1";

/** A minimal enabled rule carrying `action`, targeting `completed`. */
function mkRule(action: RuleAction): Rule {
  return {
    id: "r1",
    name: "test rule",
    enabled: true,
    trigger: { to: "completed", from: "any" },
    action,
  };
}

beforeEach(() => {
  h.delivered = [];
  h.humanWrites = [];
  // The engine resolves a session id to its terminal via `sessionIdByTmux`
  // (`th_<terminalId>` -> sessionId). Seed it so `sess1` resolves to tile `t1`.
  useSupervision.setState({
    snapshots: {},
    sessionIdByTmux: { [`th_${ID}`]: SESSION },
    statuses: {},
  });
});

describe("rules-engine funnel (PR #55 MED-1)", () => {
  it("sendText delivers through the plane with source rules-engine, never the human path", () => {
    _runRuleActionForTest(mkRule({ kind: "sendText", text: "hello" }), SESSION, "completed");
    expect(h.delivered).toEqual([[ID, "hello", "rules-engine"]]);
    expect(h.humanWrites).toEqual([]);
  });

  it("run delivers the command + CR through the plane with source rules-engine", () => {
    _runRuleActionForTest(mkRule({ kind: "run", text: "echo hi" }), SESSION, "completed");
    expect(h.delivered).toEqual([[ID, "echo hi\r", "rules-engine"]]);
    expect(h.humanWrites).toEqual([]);
  });

  it("does not write at all when the session has no live terminal", () => {
    useSupervision.setState({ snapshots: {}, sessionIdByTmux: {}, statuses: {} });
    _runRuleActionForTest(mkRule({ kind: "sendText", text: "hello" }), SESSION, "completed");
    expect(h.delivered).toEqual([]);
    expect(h.humanWrites).toEqual([]);
  });
});
