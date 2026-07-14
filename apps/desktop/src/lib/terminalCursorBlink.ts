export interface TerminalCursorContext {
  visible: boolean;
  foreground: boolean;
  tileFocused: boolean;
  terminalRegionFocused: boolean;
}

interface CursorBlinkEnvironment {
  window: Window;
  document: Document;
}

/**
 * Keeps xterm cursor animation limited to the single terminal that can receive
 * keyboard input. The controller mutates the live xterm option and never
 * recreates the terminal or changes its attachment lifecycle.
 */
export class TerminalCursorBlinkController {
  private context: TerminalCursorContext;
  private windowFocused: boolean;
  private documentVisible: boolean;
  private blinking = false;

  private readonly onInputFocus = () => {
    this.sync();
  };

  private readonly onInputBlur = () => {
    this.sync();
  };

  private readonly onWindowFocus = () => {
    this.windowFocused = true;
    this.sync();
  };

  private readonly onWindowBlur = () => {
    this.windowFocused = false;
    this.sync();
  };

  private readonly onDocumentVisibility = () => {
    this.documentVisible = this.environment.document.visibilityState === "visible";
    this.sync();
  };

  constructor(
    private readonly input: HTMLTextAreaElement,
    private readonly setCursorBlink: (enabled: boolean) => void,
    context: TerminalCursorContext,
    private readonly environment: CursorBlinkEnvironment = {
      window,
      document,
    },
  ) {
    this.context = context;
    this.windowFocused = environment.document.hasFocus();
    this.documentVisible = environment.document.visibilityState === "visible";

    input.addEventListener("focus", this.onInputFocus);
    input.addEventListener("blur", this.onInputBlur);
    environment.window.addEventListener("focus", this.onWindowFocus);
    environment.window.addEventListener("blur", this.onWindowBlur);
    environment.document.addEventListener(
      "visibilitychange",
      this.onDocumentVisibility,
    );
    this.sync();
  }

  update(context: TerminalCursorContext): void {
    this.context = context;
    this.sync();
  }

  dispose(): void {
    this.input.removeEventListener("focus", this.onInputFocus);
    this.input.removeEventListener("blur", this.onInputBlur);
    this.environment.window.removeEventListener("focus", this.onWindowFocus);
    this.environment.window.removeEventListener("blur", this.onWindowBlur);
    this.environment.document.removeEventListener(
      "visibilitychange",
      this.onDocumentVisibility,
    );
    if (this.blinking) {
      this.blinking = false;
      this.setCursorBlink(false);
    }
  }

  private sync(): void {
    const next =
      this.context.visible &&
      this.context.foreground &&
      this.context.tileFocused &&
      this.context.terminalRegionFocused &&
      this.environment.document.activeElement === this.input &&
      this.windowFocused &&
      this.documentVisible;
    if (next === this.blinking) return;
    this.blinking = next;
    this.setCursorBlink(next);
  }
}
