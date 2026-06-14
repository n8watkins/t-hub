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
import { useWorkspace } from "../store/workspace";
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
  const fitRef = useRef<FitAddon | null>(null);
  // Skips the zoom effect's first (mount) run so it doesn't double-fit on open.
  const zoomMountRef = useRef(true);
  // Global zoom: every tile reads the same font size so they scale together.
  const fontSize = useWorkspace((s) => s.fontSize);

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
    let promptTimer: ReturnType<typeof setTimeout> | null = null;
    let rafId = 0;

    const term = new Terminal({
      allowProposedApi: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, "JetBrains Mono", monospace',
      fontSize: useWorkspace.getState().fontSize,
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
    fitRef.current = fit;
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

    // Match the user's Windows Terminal bindings: Ctrl+C copies the selection
    // (and clears it) or, with nothing selected, falls through to the shell as
    // SIGINT; Ctrl+V pastes (bracketed-paste aware via term.paste). Ctrl +/-/0
    // zoom is handled here too because a focused xterm otherwise swallows those
    // before they reach the window-level handler. Returning false stops xterm
    // from sending the key to the PTY; stopPropagation prevents the Canvas
    // window handler from double-firing.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const mod = e.ctrlKey || e.metaKey;
      if (!mod || e.altKey) return true;
      const key = e.key.toLowerCase();

      if (key === "c") {
        if (term.hasSelection()) {
          void navigator.clipboard.writeText(term.getSelection()).catch(() => {});
          term.clearSelection();
          e.preventDefault();
          e.stopPropagation();
          return false;
        }
        return true; // no selection -> let Ctrl+C reach the shell as SIGINT
      }
      if (key === "v") {
        void navigator.clipboard
          .readText()
          .then((t) => {
            if (t) term.paste(t);
          })
          .catch(() => {});
        e.preventDefault();
        e.stopPropagation();
        return false;
      }
      if (key === "=" || key === "+") {
        useWorkspace.getState().zoomIn();
        e.preventDefault();
        e.stopPropagation();
        return false;
      }
      if (key === "-" || key === "_") {
        useWorkspace.getState().zoomOut();
        e.preventDefault();
        e.stopPropagation();
        return false;
      }
      if (key === "0") {
        useWorkspace.getState().zoomReset();
        e.preventDefault();
        e.stopPropagation();
        return false;
      }
      return true;
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

    // Defer the first fit until the browser has completed the constrained-flex
    // layout pass (and WebGL has loaded). A synchronous fit here reads a
    // transient/unconstrained height and oversizes the grid. Attaching from
    // inside the rAF means the backend PTY is created at the settled geometry,
    // so there is no 80x24 -> real-size redraw trail. Double-rAF reliably lands
    // after layout + paint.
    rafId = requestAnimationFrame(() => {
      rafId = requestAnimationFrame(() => {
        if (disposed) return;
        try {
          fit.fit();
        } catch {
          /* container detached; ignore */
        }
        void (async () => {
          try {
            const scrollback = await attachTerminal(
              terminalId,
              term.cols,
              term.rows,
            );
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

            // A fresh pane's prompt may have been printed before we subscribed
            // (so it never streamed to us), and a tiny resize-redraw trail can
            // leave duplicate prompts. Now that we're attached + subscribed,
            // nudge zsh to clear+redraw one clean prompt. Ctrl-L (\x0c) only
            // redraws on an otherwise-empty fresh terminal.
            promptTimer = setTimeout(() => {
              if (!disposed) void writeTerminal(terminalId, "\x0c");
            }, 350);
          } catch {
            // attach failed (e.g. session gone); leave the tile rendered but inert.
          }
        })();
      });
    });

    // Debounced resize → keep PTY columns/rows in sync with the tile size.
    resizeObserver = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(pushResize, 50);
    });
    resizeObserver.observe(container);

    return () => {
      disposed = true;
      initializedRef.current = false;

      cancelAnimationFrame(rafId);
      if (promptTimer) clearTimeout(promptTimer);
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
      fitRef.current = null;
    };
  }, [terminalId, visible]);

  // Apply global zoom changes live, without recreating the terminal. Skips the
  // first (mount) run -- the init effect already fits once, so fitting again
  // here would double-fit and trigger a redundant SIGWINCH / prompt redraw.
  useEffect(() => {
    if (zoomMountRef.current) {
      zoomMountRef.current = false;
      return;
    }
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = fontSize;
    try {
      fitRef.current?.fit();
      void resizeTerminal(terminalId, term.cols, term.rows);
    } catch {
      /* container detached mid-zoom; ignore */
    }
  }, [fontSize, terminalId]);

  return <div ref={containerRef} className="termhub-terminal h-full w-full" />;
}
