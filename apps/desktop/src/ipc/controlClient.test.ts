import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

import {
  ControlRequestError,
  controlRequest,
  isRetryableControlError,
} from "./controlClient";

beforeEach(() => {
  mocks.invoke.mockReset();
});

describe("controlRequest", () => {
  it("preserves a structured retryable backend rejection", async () => {
    mocks.invoke.mockRejectedValue({
      message: "history_resume_failed: placement remained uncertain",
      retryable: true,
      kind: "history_resume_failed",
      details: { phase: "placement" },
    });

    const failure = await controlRequest("history_resume").catch((reason) => reason);

    expect(failure).toBeInstanceOf(ControlRequestError);
    expect(failure).toMatchObject({
      message: "history_resume_failed: placement remained uncertain",
      retryable: true,
      kind: "history_resume_failed",
      details: { phase: "placement" },
    });
    expect(isRetryableControlError(failure)).toBe(true);
  });

  it("does not promote a definitive backend rejection to retryable", async () => {
    mocks.invoke.mockRejectedValue({
      message: "history_unavailable: conversation is not resumable",
      retryable: false,
    });

    const failure = await controlRequest("history_resume").catch((reason) => reason);

    expect(failure).toBeInstanceOf(ControlRequestError);
    expect(isRetryableControlError(failure)).toBe(false);
  });
});
