import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalCursorBlinkController } from "./terminalCursorBlink";

function setDocumentState(focused: boolean, visibility: DocumentVisibilityState) {
  vi.spyOn(document, "hasFocus").mockReturnValue(focused);
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: visibility,
  });
}

function activeContext() {
  return {
    visible: true,
    foreground: true,
    tileFocused: true,
    terminalRegionFocused: true,
  };
}

describe("TerminalCursorBlinkController", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.body.replaceChildren();
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
  });

  it("blinks only while the active terminal input owns keyboard focus", () => {
    setDocumentState(true, "visible");
    const input = document.createElement("textarea");
    document.body.append(input);
    const changes: boolean[] = [];
    const controller = new TerminalCursorBlinkController(
      input,
      (enabled) => changes.push(enabled),
      activeContext(),
    );

    expect(changes).toEqual([]);
    input.focus();
    expect(changes).toEqual([true]);
    input.blur();
    expect(changes).toEqual([true, false]);

    controller.dispose();
  });

  it("disables blinking when focus moves to another tile or UI region", () => {
    setDocumentState(true, "visible");
    const input = document.createElement("textarea");
    document.body.append(input);
    input.focus();
    const changes: boolean[] = [];
    const controller = new TerminalCursorBlinkController(
      input,
      (enabled) => changes.push(enabled),
      activeContext(),
    );

    expect(changes).toEqual([true]);
    controller.update({ ...activeContext(), tileFocused: false });
    controller.update({ ...activeContext(), terminalRegionFocused: false });
    controller.update(activeContext());
    expect(changes).toEqual([true, false, true]);

    controller.dispose();
  });

  it("disables blinking for parked panel, overlay, and inactive-tab terminals", () => {
    setDocumentState(true, "visible");
    const input = document.createElement("textarea");
    document.body.append(input);
    input.focus();
    const changes: boolean[] = [];
    const controller = new TerminalCursorBlinkController(
      input,
      (enabled) => changes.push(enabled),
      activeContext(),
    );

    controller.update({ ...activeContext(), foreground: false });
    controller.update(activeContext());
    controller.update({ ...activeContext(), visible: false });
    expect(changes).toEqual([true, false, true, false]);

    controller.dispose();
  });

  it("tracks window focus and document visibility transitions", () => {
    setDocumentState(true, "visible");
    const input = document.createElement("textarea");
    document.body.append(input);
    input.focus();
    const changes: boolean[] = [];
    const controller = new TerminalCursorBlinkController(
      input,
      (enabled) => changes.push(enabled),
      activeContext(),
    );

    window.dispatchEvent(new FocusEvent("blur"));
    window.dispatchEvent(new FocusEvent("focus"));
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "hidden",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    expect(changes).toEqual([true, false, true, false, true]);

    controller.dispose();
  });

  it("removes listeners and leaves the cursor disabled when disposed", () => {
    setDocumentState(true, "visible");
    const input = document.createElement("textarea");
    document.body.append(input);
    input.focus();
    const changes: boolean[] = [];
    const controller = new TerminalCursorBlinkController(
      input,
      (enabled) => changes.push(enabled),
      activeContext(),
    );

    controller.dispose();
    input.blur();
    input.focus();
    window.dispatchEvent(new FocusEvent("focus"));
    expect(changes).toEqual([true, false]);
  });
});
