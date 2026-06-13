// xterm.js terminal tile for the 0.1 terminal nucleus.
//
// Responsibilities (PRD §9.1, FR-004/FR-005, §12.1):
//   - Create an xterm.js Terminal with Fit + WebGL + Search + Unicode11 addons.
//   - On mount/visible: attachTerminal(id, cols, rows), write the base64 scrollback,
//     subscribe onOutput -> xterm.write(decodeBase64(...)).
//   - xterm.onData -> writeTerminal(id, data); ResizeObserver/FitAddon -> resizeTerminal.
//   - Dispose cleanly on unmount; mount only when `visible` (hidden tabs detach).
//
// Lifecycle is keyed on [terminalId, visible]. Hidden tiles fully tear down their
// xterm instance + PTY-client subscriptions so a wall of tiles stays cheap; the
// tmux session keeps running backend-side, and re-attaching replays scrollback.
import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  attachTerminal,
  decodeBase64,
  onExit,
  onOutput,
  resizeTerminal,
  writeTerminal,
} from "../ipc/client";
import type { TerminalId } from "../ipc/types";
import "./Terminal.css";

export interface TerminalViewProps {
  terminalId: TerminalId;
  /** Mount xterm only when visible; hidden tiles detach their PTY client. */
  visible: boolean;
}

export function TerminalView({
  terminalId,
  visible,
}: TerminalViewProps): JSX.Element | null {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  // Guards against a second init for the same (id, visible) effect run even
  // though main.tsx omits StrictMode — belt-and-braces against double `open()`.
  const initializedRef = useRef(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!visible || !container || initializedRef.current) return;
    initializedRef.current = true;

    // Disposables collected during async setup; cleanup awaits/runs them all
    // even if the effect is torn down before setup finishes (fast tab flips).
    const unlisteners: UnlistenFn[] = [];
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    let webgl: WebglAddon | null = null;
    let webglContextLoss: { dispose(): void } | null = null;
    let disposed = false;

    const term = new Terminal({
      allowProposedApi: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, "JetBrains Mono", monospace',
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      theme: { background: "#0a0a0a" },
    });
    termRef.current = term;

    // Unicode 11 width tables must be loaded + selected before output is written
    // so wide glyphs / emoji line up with what the PTY computed.
    const unicode11 = new Unicode11Addon();
    term.loadAddon(unicode11);
    term.unicode.activeVersion = "11";

    const fit = new FitAddon();
    term.loadAddon(fit);
    const search = new SearchAddon();
    term.loadAddon(search);

    term.open(container);

    // WebGL renderer is best-effort. Some GPUs/driver states refuse a context;
    // on loss we drop the addon and xterm transparently falls back to canvas.
    try {
      webgl = new WebglAddon();
      webglContextLoss = webgl.onContextLoss(() => {
        webgl?.dispose();
        webgl = null;
      });
      term.loadAddon(webgl);
    } catch {
      webgl?.dispose();
      webgl = null;
    }

    // Forward keystrokes/paste to the PTY.
    const dataSub = term.onData((d) => {
      void writeTerminal(terminalId, d);
    });

    const pushResize = () => {
      if (disposed) return;
      try {
        fit.fit();
        void resizeTerminal(terminalId, term.cols, term.rows);
      } catch {
        // Container may be detached mid-resize; ignore.
      }
    };

    // Initial sizing before attach so the backend PTY starts at the right size.
    fit.fit();

    void (async () => {
      try {
        const scrollback = await attachTerminal(terminalId, term.cols, term.rows);
        if (disposed) return;
        term.write(decodeBase64(scrollback));

        const offOutput = await onOutput((e) => {
          if (e.id === terminalId) term.write(decodeBase64(e.base64));
        });
        if (disposed) {
          void offOutput();
          return;
        }
        unlisteners.push(offOutput);

        const offExit = await onExit((e) => {
          if (e.id === terminalId) term.writeln("\r\n[process exited]");
        });
        if (disposed) {
          void offExit();
          return;
        }
        unlisteners.push(offExit);
      } catch {
        // attach failed (e.g. session gone); leave the tile rendered but inert.
      }
    })();

    // Debounced resize → keep PTY columns/rows in sync with the tile size.
    resizeObserver = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(pushResize, 50);
    });
    resizeObserver.observe(container);

    return () => {
      disposed = true;
      initializedRef.current = false;

      if (resizeTimer) clearTimeout(resizeTimer);
      resizeObserver?.disconnect();
      resizeObserver = null;

      dataSub.dispose();
      webglContextLoss?.dispose();

      // Await all event unlisteners so no stray onOutput fires into a disposed
      // term. Any subscriptions still in-flight bail via the `disposed` flag.
      void Promise.all(unlisteners.map((un) => un())).catch(() => {
        /* ignore unlisten races */
      });
      unlisteners.length = 0;

      term.dispose();
      termRef.current = null;
    };
  }, [terminalId, visible]);

  return <div ref={containerRef} className="termhub-terminal h-full w-full" />;
}
