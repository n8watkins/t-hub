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
import nativeControlErrors from "./fixtures/native-control-error-bridge.json";

beforeEach(() => {
  mocks.invoke.mockReset();
});

describe("controlRequest", () => {
  it("preserves native serialized bridge errors without reconstructing them", async () => {
    for (const response of Object.values(nativeControlErrors)) {
      mocks.invoke.mockRejectedValueOnce(response);
      const failure = await controlRequest("native_dispatcher_command").catch((reason) => reason);
      expect(failure).toBeInstanceOf(ControlRequestError);
      expect(failure).toMatchObject(response);
    }
  });

  it("preserves a structured retryable backend rejection", async () => {
    mocks.invoke.mockRejectedValue(nativeControlErrors.retryable);

    const failure = await controlRequest("history_resume").catch((reason) => reason);

    expect(failure).toBeInstanceOf(ControlRequestError);
    expect(failure).toMatchObject({
      ...nativeControlErrors.retryable,
    });
    expect(isRetryableControlError(failure)).toBe(true);
  });

  it("does not promote a definitive backend rejection to retryable", async () => {
    mocks.invoke.mockRejectedValue(nativeControlErrors.validation);

    const failure = await controlRequest("history_resume").catch((reason) => reason);

    expect(failure).toBeInstanceOf(ControlRequestError);
    expect(isRetryableControlError(failure)).toBe(false);
  });
});
