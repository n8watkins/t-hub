// Strict chime policy: a notification (chime + optional OS toast) fires ONLY for
// the attention-worthy edges the general cares about — a decision needed
// (needsQuestion / needsPermission), a finish (completed), a blocker (failed) —
// mirroring the voice-announce doctrine. Everything else, INCLUDING rateLimited
// and all routine/working states, stays silent (null).
import { describe, it, expect } from "vitest";
import { statusToNotification } from "./notify";
import type { SessionStatus } from "../ipc/model";

describe("statusToNotification — strict chime policy", () => {
  it("chimes for the four attention-worthy edges", () => {
    expect(statusToNotification("needsQuestion")?.kind).toBe("attention");
    expect(statusToNotification("needsPermission")?.kind).toBe("attention");
    expect(statusToNotification("completed")?.kind).toBe("done");
    expect(statusToNotification("failed")?.kind).toBe("error");
  });

  it("stays SILENT for routine / non-attention states (incl. rateLimited)", () => {
    const silent: SessionStatus[] = [
      "rateLimited",
      "working",
      "waitingOnSubagents",
      "detached",
      "restoring",
      "expired",
      "unknown",
    ];
    for (const s of silent) expect(statusToNotification(s)).toBeNull();
  });

  it("names the captain in the title/body when a subject is given", () => {
    const n = statusToNotification("needsPermission", "Captain alpha");
    expect(n?.title).toBe("Captain alpha needs permission");
    expect(n?.body).toContain("Captain alpha");
  });

  it("keeps generic wording when no subject is given (non-captain sessions)", () => {
    const n = statusToNotification("needsQuestion");
    expect(n?.title).toBe("Claude needs an answer");
    expect(n?.body).toBe("A session is waiting on your input.");
  });
});
