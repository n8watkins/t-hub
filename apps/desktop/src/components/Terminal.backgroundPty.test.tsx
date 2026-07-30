import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({
  attachTerminal: vi.fn(async () => ""),
  closeTerminal: vi.fn(async () => {}),
  listTerminals: vi.fn(async () => [
    {
      id: "term",
      title: "Shell",
      cwd: "/home/test",
      state: "live",
    },
  ]),
  onExit: vi.fn(async () => () => {}),
  onOutput: vi.fn(async () => () => {}),
}));

vi.mock("../ipc/client", () => ({
  ...ipc,
  decodeBase64: () => new Uint8Array(),
  isMissingLiveTerminalError: () => false,
  resizeTerminal: vi.fn(async () => {}),
  writeTerminal: vi.fn(async () => {}),
}));
vi.mock("../ipc/client05", () => ({
  clipboardImageToTemp: vi.fn(async () => null),
  tmuxExitScroll: vi.fn(async () => {}),
  tmuxScroll: vi.fn(async () => {}),
}));
vi.mock("../lib/clipboard", () => ({
  clipboardRead: vi.fn(async () => ""),
  clipboardWrite: vi.fn(async () => {}),
}));
vi.mock("../lib/dropPaste", () => ({
  formatPathsForInsert: (paths: string[]) => paths.join(" "),
  installFileDropOnce: vi.fn(),
}));
vi.mock("../lib/diag", async (importOriginal) => ({
  // Spread the REAL module rather than enumerating exports: this mock has been
  // broken twice by a NEW diag export (`dmark`, then `diagEnabled`), because a
  // missing key makes any call throw from inside a component render.
  ...(await importOriginal<typeof import("../lib/diag")>()),
  tlog: vi.fn(),
  dmark: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn(async () => {}) }));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options: Record<string, unknown> = {};
    unicode = { activeVersion: "" };
    textarea = document.createElement("textarea");
    buffer = {
      active: {
        getLine: () => undefined,
        type: "normal",
      },
    };

    loadAddon(): void {}
    open(container: HTMLElement): void {
      container.append(this.textarea);
    }
    registerLinkProvider(): { dispose: () => void } {
      return { dispose: () => {} };
    }
    onSelectionChange(): { dispose: () => void } {
      return { dispose: () => {} };
    }
    onData(): { dispose: () => void } {
      return { dispose: () => {} };
    }
    attachCustomKeyEventHandler(): void {}
    hasSelection(): boolean {
      return false;
    }
    getSelection(): string {
      return "";
    }
    clearSelection(): void {}
    paste(): void {}
    write(
      _data: string | Uint8Array,
      callback?: () => void,
    ): void {
      callback?.();
    }
    reset(): void {}
    refresh(): void {}
    clearTextureAtlas(): void {}
    focus(): void {}
    dispose(): void {}
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit(): void {}
    proposeDimensions(): { cols: number; rows: number } {
      return { cols: 80, rows: 24 };
    }
  },
}));
vi.mock("@xterm/addon-search", () => ({
  SearchAddon: class {},
}));
vi.mock("@xterm/addon-unicode11", () => ({
  Unicode11Addon: class {},
}));
vi.mock("@xterm/addon-web-links", () => ({
  WebLinksAddon: class {},
}));

import { TerminalView } from "./Terminal";
import {
  getTerminalResourceSnapshot,
  resetTerminalResourcesForTests,
} from "../lib/terminalResources";
import { resetTerminalDetachmentsForTests } from "../lib/terminalLifecycle";
import { useWorkspace } from "../store/workspace";

class ResizeObserverStub {
  observe(): void {}
  disconnect(): void {}
}

describe("TerminalView background PTY lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    ipc.attachTerminal.mockClear();
    ipc.closeTerminal.mockClear();
    ipc.listTerminals.mockClear();
    ipc.onExit.mockClear();
    ipc.onOutput.mockClear();
    resetTerminalResourcesForTests();
    resetTerminalDetachmentsForTests();
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    vi.stubGlobal(
      "requestAnimationFrame",
      (callback: FrameRequestCallback) => {
        callback(performance.now());
        return 1;
      },
    );
    vi.stubGlobal("cancelAnimationFrame", () => {});
    useWorkspace.setState({
      tabs: [{ id: "work", name: "Work", kind: "work", order: ["term"] }],
      activeTabId: "work",
      focusedId: "term",
      terminals: {
        term: {
          id: "term",
          tmuxSession: "th_term",
          title: "Shell",
          cwd: "/home/test",
          state: "live",
        },
      },
      poppedOutTabs: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("parks only the PTY after two seconds, keeps xterm warm, and reattaches on return", async () => {
    const view = render(
      <TerminalView terminalId="term" visible foreground />,
    );

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(ipc.attachTerminal).toHaveBeenCalledTimes(1);
    expect(getTerminalResourceSnapshot()).toMatchObject({
      xterms: 1,
      ptys: 1,
    });

    view.rerender(
      <TerminalView terminalId="term" visible foreground={false} />,
    );
    act(() => vi.advanceTimersByTime(1999));
    expect(ipc.closeTerminal).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
      await Promise.resolve();
    });
    expect(ipc.closeTerminal).toHaveBeenCalledTimes(1);
    expect(getTerminalResourceSnapshot()).toMatchObject({
      xterms: 1,
      ptys: 0,
    });
    expect(ipc.listTerminals).not.toHaveBeenCalled();

    // UNPARK IS A FAST PATH, not attach-loss recovery. We parked this tile
    // ourselves, so returning to it must not pay the fault ceremony: no
    // RECONNECT_INITIAL_MS backoff before the attach, and no per-tile
    // `list_terminals` liveness pre-probe (`attach_terminal` verifies liveness
    // server-side, and a genuinely dead session makes it throw, which downgrades
    // to the slow path where the probe does run before declaring the tile dead).
    //
    // Note the timers are NOT advanced here - only microtasks are drained. That
    // is the assertion: the attach lands without any timer at all. Before this,
    // every workspace switch waited 250ms per tile and printed
    // "[session detached - reconnecting...]" into each returning pane.
    view.rerender(
      <TerminalView terminalId="term" visible foreground />,
    );
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(ipc.attachTerminal).toHaveBeenCalledTimes(2);
    expect(ipc.listTerminals).not.toHaveBeenCalled();
    const foregroundResources = getTerminalResourceSnapshot();
    expect(foregroundResources).toMatchObject({
      xterms: 1,
      ptys: 1,
    });

    if (process.env.NO_MISTAKES_EVIDENCE === "1") {
      console.info(
        "BACKGROUND_PTY_EVIDENCE",
        JSON.stringify({
          switchGraceMs: 2000,
          beforeDeadline: { closeCalls: 0 },
          parked: {
            closeCalls: 1,
            xterms: 1,
            ptys: 0,
            livenessChecks: 0,
          },
          foregrounded: {
            attachCalls: ipc.attachTerminal.mock.calls.length,
            livenessChecks: ipc.listTerminals.mock.calls.length,
            xterms: foregroundResources.xterms,
            ptys: foregroundResources.ptys,
          },
        }),
      );
    }
  });

  // The fast unpark is only safe because a FAILED optimistic attach gives up its
  // assumptions. Once the attach throws we no longer know the session is alive,
  // so the loop must downgrade to the full recovery path - back off, verify
  // liveness, and only then declare anything - rather than spinning attaches
  // against a session that may be gone.
  it("downgrades a failed fast unpark to backoff + liveness verification", async () => {
    const view = render(
      <TerminalView terminalId="term" visible foreground />,
    );
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(ipc.attachTerminal).toHaveBeenCalledTimes(1);

    // Park it.
    view.rerender(
      <TerminalView terminalId="term" visible foreground={false} />,
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
      await Promise.resolve();
    });
    expect(ipc.closeTerminal).toHaveBeenCalledTimes(1);
    expect(ipc.listTerminals).not.toHaveBeenCalled();

    // The unpark attach fails once, then succeeds.
    ipc.attachTerminal.mockRejectedValueOnce(new Error("attach refused"));

    view.rerender(
      <TerminalView terminalId="term" visible foreground />,
    );
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    // The fast attempt ran immediately (no timer) and failed. Crucially it has
    // NOT yet retried: the downgrade re-introduces the backoff, so nothing more
    // happens until the timer advances.
    expect(ipc.attachTerminal).toHaveBeenCalledTimes(2);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
      await Promise.resolve();
      await Promise.resolve();
    });
    // Slow path now: liveness verified before the retry, and the retry landed.
    expect(ipc.listTerminals).toHaveBeenCalled();
    expect(ipc.attachTerminal).toHaveBeenCalledTimes(3);
    expect(getTerminalResourceSnapshot()).toMatchObject({
      xterms: 1,
      ptys: 1,
    });
  });
});
