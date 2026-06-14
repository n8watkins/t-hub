// xterm.js terminal tile for the 0.1 terminal nucleus.
//
// RENDERER (mutedbug fix): we deliberately do NOT load the WebGL addon. Each
// xterm WebGL addon opens its OWN WebGL context, and WebView2 (Chromium) caps
// the number of simultaneously-live WebGL contexts and evicts the
// least-recently-used ones under GPU/memory pressure. With 6+ terminals (each a
// context) plus a relayout/repaint (e.g. clicking a tile that was repositioned
// while unfocused), WebView2 would evict contexts across the grid. xterm's
// WebGL addon responds to the browser's `webglcontextlost` event by calling
// preventDefault() and waiting a HARD-CODED 3000ms before firing its
// onContextLoss fallback -- during which every evicted canvas is blank with
// nothing repainting it. That is the "all terminals go blank / uniform muted
// gray, then a tab switch resets them" symptom. Using xterm's default DOM
// renderer (no GPU context) removes the ceiling entirely, so the eviction --
// and the blanking -- cannot happen. For 6-12 terminals the DOM renderer is
// plenty fast, and it also sidesteps WebView2 driver-state context refusals and
// the stale-frame-on-move class of bugs.
//
// Responsibilities (PRD §9.1, FR-004/FR-005, §12.1):
//   - Create an xterm.js Terminal with Fit + Search + Unicode11 addons.
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
import { useTheme, type TerminalPalette } from "../store/theme";
import { tlog } from "../lib/diag";
import { REPAINT_ALL_EVENT } from "../lib/repaint";
import type { ITheme } from "@xterm/xterm";
import "./Terminal.css";

/** Default xterm theme when the active theme carries no terminal palette. */
const DEFAULT_TERM_THEME: ITheme = { background: "#0a0a0a" };

/** Map TermHub's TerminalPalette onto xterm's ITheme (default when absent). */
function toXtermTheme(p: TerminalPalette | undefined): ITheme {
  if (!p) return DEFAULT_TERM_THEME;
  return {
    background: p.background,
    foreground: p.foreground,
    cursor: p.cursor,
    cursorAccent: p.background,
    selectionBackground: p.selection,
    black: p.ansi.black,
    red: p.ansi.red,
    green: p.ansi.green,
    yellow: p.ansi.yellow,
    blue: p.ansi.blue,
    magenta: p.ansi.magenta,
    cyan: p.ansi.cyan,
    white: p.ansi.white,
    brightBlack: p.ansi.brightBlack,
    brightRed: p.ansi.brightRed,
    brightGreen: p.ansi.brightGreen,
    brightYellow: p.ansi.brightYellow,
    brightBlue: p.ansi.brightBlue,
    brightMagenta: p.ansi.brightMagenta,
    brightCyan: p.ansi.brightCyan,
    brightWhite: p.ansi.brightWhite,
  };
}

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
  // Live terminal palette from the active theme (undefined => xterm defaults).
  const termPalette = useTheme((s) => s.active.terminal);

  useEffect(() => {
    const container = containerRef.current;
    if (!visible || !container || initializedRef.current) return;
    initializedRef.current = true;

    // Disposables collected during async setup; cleanup awaits/runs them all
    // even if the effect is torn down before setup finishes (fast tab flips).
    const unlisteners: UnlistenFn[] = [];
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;
    let promptTimer: ReturnType<typeof setTimeout> | null = null;
    let rafId = 0;

    const term = new Terminal({
      allowProposedApi: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, "JetBrains Mono", monospace',
      fontSize: useWorkspace.getState().fontSize,
      cursorBlink: true,
      scrollback: 5000,
      theme: toXtermTheme(useTheme.getState().active.terminal),
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

    // No WebGL addon: xterm uses its default DOM renderer. See the file header
    // (mutedbug fix) for why -- a per-terminal WebGL context hits WebView2's
    // context ceiling and blanks the whole grid on eviction. The DOM renderer
    // has no GPU context, so that failure mode does not exist.
    term.open(container);

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

    // Tracks the column count we last reported to the PTY. A width change is the
    // only thing that makes xterm REFLOW (re-wrap) its buffer, so we use a
    // before/after column comparison in the settle handler to decide whether the
    // garbled reflowed scrollback needs clearing (see settleResize).
    let lastCols = 0;

    const pushResize = () => {
      if (disposed) return;
      try {
        fit.fit();
        void resizeTerminal(terminalId, term.cols, term.rows);
      } catch {
        // Container may be detached mid-resize; ignore.
      }
    };

    // Runs ONCE after a resize has settled (debounced). A continuous window/grid
    // drag fires the ResizeObserver many times; we only get here after motion
    // stops, so the PTY sees a single resize/SIGWINCH instead of a stream.
    //
    // The corruption fix: the terminals draw inline (not alt-screen), so the
    // TUI's prior frames live in xterm's scrollback. On a WIDTH change xterm
    // reflows that whole buffer and those frames re-wrap into a duplicated,
    // scrambled mess. After the settle-fit, tmux gets the SIGWINCH and redraws
    // the CURRENT screen cleanly at the new width -- so the old reflowed history
    // is pure garbage we can safely drop. term.clear() discards the entire
    // scrollback while KEEPING the cursor's line as the new first line, i.e. it
    // leaves the live screen the user is reading intact and only kills the
    // duplicated history above it. We only clear when the column count actually
    // changed (height-only changes / re-shows at the same width don't reflow),
    // so a pure vertical resize never throws away readable scrollback.
    const settleResize = () => {
      if (disposed) return;
      const before = term.cols;
      pushResize();
      const widthChanged = term.cols !== before || term.cols !== lastCols;
      lastCols = term.cols;
      // Defer clear + repaint to the next frame so tmux's post-SIGWINCH redraw
      // has a chance to land first; clearing only removes scrollback above the
      // live line, and the forced repaint then shows a clean, single frame.
      if (widthChanged) {
        requestAnimationFrame(() => {
          if (disposed) return;
          try {
            term.clear();
          } catch {
            // Renderer/buffer detached mid-call; ignore.
          }
          forceRepaint();
        });
      } else {
        forceRepaint();
      }
    };

    // Defer the first fit until the browser has completed the constrained-flex
    // layout pass. A synchronous fit here reads a
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
            // STARTUP-FREEZE FIX (BUG 1): subscribe to terminal://output BEFORE
            // requesting attach, so the backend never streams PTY bytes into a
            // callback that doesn't exist yet. The OLD ordering attached first
            // (which spawns the reader thread and starts emitting immediately),
            // THEN registered onOutput -- so on a cold relaunch with ~16
            // terminals all attaching at once, the readers flooded OUTPUT events
            // at callback ids the page hadn't registered (or had torn down during
            // the mount churn), producing the thousands of "[TAURI] Couldn't find
            // callback id N" warnings and a grid that rendered blank/frozen until
            // a manual reload (by which time Rust was idle, so no race).
            //
            // Because the listener is now live BEFORE the seed/scrollback is
            // captured, live bytes can arrive while we're still awaiting the
            // attach response. We BUFFER those into `liveBuffer` and only start
            // writing to xterm directly once the seed has been written, then
            // flush the buffer -- so history (seed) and live output stay correctly
            // ordered with no duplication and no lost bytes.
            let seeded = false;
            const liveBuffer: Uint8Array[] = [];

            const offOutput = await onOutput((e) => {
              if (e.id !== terminalId || disposed) return;
              const bytes = decodeBase64(e.base64);
              if (seeded) term.write(bytes);
              else liveBuffer.push(bytes);
            });
            if (disposed) {
              void offOutput();
              return;
            }
            unlisteners.push(offOutput);

            const offExit = await onExit((e) => {
              if (e.id === terminalId && !disposed)
                term.writeln("\r\n[process exited]");
            });
            if (disposed) {
              void offExit();
              return;
            }
            unlisteners.push(offExit);

            tlog(
              "attach",
              `subscribed ${terminalId} (listeners live BEFORE attach); requesting attach ${term.cols}x${term.rows}`,
            );

            const scrollback = await attachTerminal(
              terminalId,
              term.cols,
              term.rows,
            );
            if (disposed) return;
            // Empty seed => fresh spawn (backend skips capture); non-empty =>
            // reattach history to restore. Only write a real seed; a fresh
            // prompt is drawn by the forced redraw below.
            const seed = decodeBase64(scrollback);
            const freshSpawn = seed.length === 0;
            if (!freshSpawn) term.write(seed);

            // Seed is on screen; switch to live writes and flush anything that
            // arrived on the listener while we were awaiting attach/seed.
            seeded = true;
            if (liveBuffer.length > 0) {
              for (const chunk of liveBuffer) term.write(chunk);
              tlog(
                "attach",
                `attached ${terminalId}: seed ${seed.length}B, flushed ${liveBuffer.length} buffered live chunk(s)`,
              );
              liveBuffer.length = 0;
            } else {
              tlog(
                "attach",
                `attached ${terminalId}: seed ${seed.length}B, no buffered live chunks`,
              );
            }

            // On a fresh spawn we seeded nothing, so draw one clean prompt: send
            // Ctrl-L (\x0c) once subscribed. If zsh is still loading it buffers
            // the keystroke and redraws when ready, so the prompt always appears
            // -- with no seed reflow cascade. Reattach already restored history.
            if (freshSpawn) {
              promptTimer = setTimeout(() => {
                if (!disposed) void writeTerminal(terminalId, "\x0c");
              }, 250);
            }
          } catch {
            // attach failed (e.g. session gone); leave the tile rendered but inert.
          }
        })();
      });
    });

    // Force xterm's renderer to repaint the whole viewport. The DOM renderer
    // (like the GPU ones before it) doesn't redraw a frame when the element
    // merely goes hidden->visible (or the window settles after a resize) at the
    // SAME size, because nothing wrote new cells and no fit/SIGWINCH fired -- so
    // the box can show a stale/blank frame until something dirties it (e.g. a
    // click). `term.refresh(0, rows-1)` marks every line dirty and forces a
    // fresh frame. Cheap and idempotent; safe to call whenever the box reappears.
    const forceRepaint = () => {
      if (disposed) return;
      const t = termRef.current;
      if (!t) return;
      // Diagnostic (mutedbug): a repaint requested while the terminal has no
      // rows means there's no buffer to draw -- it would render blank no matter
      // what, so flag it to keep a devtools session honest.
      if (t.rows <= 0) {
        console.warn(
          `[termhub] forceRepaint on terminal ${terminalId} with no buffer (rows=${t.rows})`,
        );
        return;
      }
      try {
        t.refresh(0, t.rows - 1);
      } catch {
        // Renderer detached mid-call; ignore.
      }
    };

    // The pool parks inactive/cross-tab terminals offscreen (translate
    // -100000px) + visibility:hidden, and brings the active tab's terminal back
    // onscreen on a tab switch. That offscreen<->onscreen move is a geometric
    // change an IntersectionObserver reports (visibility:hidden alone is not,
    // but the transform park is), so this fires exactly when a pooled terminal
    // becomes the visible active tab again -- the moment its WebGL canvas would
    // otherwise read blank. We repaint on the NEXT frame so it lands after the
    // pool's useLayoutEffect has settled the box. ADDITIVE: this never fits or
    // attaches, so it can't perturb the first-fit/prompt cascade timing.
    let visObserver: IntersectionObserver | null = null;
    if (typeof IntersectionObserver !== "undefined") {
      visObserver = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (entry.isIntersecting) {
              requestAnimationFrame(forceRepaint);
            }
          }
        },
        { threshold: 0 },
      );
      visObserver.observe(container);
    }

    // Belt-and-suspenders repaint on an on-screen REPOSITION. The pool dispatches
    // a "th-pool-moved" event on this terminal's wrapper whenever it repositions
    // a VISIBLE terminal to a new transform (a same-tab reorder/swap). That move
    // keeps the terminal on-screen, so the IntersectionObserver above never fires
    // -- and a moved WebGL canvas can read stale/blank until something dirties it.
    // We repaint on the next frame (after the pool's layout sync settles the box).
    // ADDITIVE: never fits or attaches, so the first-fit/prompt cascade is intact.
    const wrapEl = container.closest("[data-th-pool-tile]") as HTMLElement | null;
    const onPoolMoved = () => requestAnimationFrame(forceRepaint);
    wrapEl?.addEventListener("th-pool-moved", onPoolMoved);

    // Repaint when ANY full-screen overlay (spawn-preset menu, file/web preview,
    // Settings) opens or closes. WebView2 leaves the DOM-rendered terminals on a
    // stale/blank frame when a new `fixed` layer is added over them or removed,
    // and nothing moves or resizes — so the two repaint paths above never fire.
    // repaintAllTerminals() broadcasts on every overlay toggle; we redraw on the
    // next frame, after that overlay change has painted. See src/lib/repaint.ts.
    const onRepaintAll = () => requestAnimationFrame(forceRepaint);
    window.addEventListener(REPAINT_ALL_EVENT, onRepaintAll);

    // Debounced resize -> keep PTY columns/rows in sync with the tile size, but
    // only ONCE the drag SETTLES. A continuous window/gutter drag fires this
    // observer (and the pool's per-placeholder one) rapidly; firing a PTY resize
    // per step sends a stream of SIGWINCHs, each making the inline TUI redraw a
    // full frame into the scrollback -- those frames then reflow into the
    // duplicated/scrambled mess on every width step and compound. A ~250ms tail
    // coalesces the whole drag into a single resize + single tmux redraw, and
    // settleResize then drops the reflowed garbage (see its comment). 250ms is
    // long enough that a fast continuous drag never trips it mid-motion yet
    // short enough to feel immediate once the user lets go.
    resizeObserver = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(settleResize, 250);
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
      visObserver?.disconnect();
      visObserver = null;
      wrapEl?.removeEventListener("th-pool-moved", onPoolMoved);
      window.removeEventListener(REPAINT_ALL_EVENT, onRepaintAll);

      dataSub.dispose();

      // Await all event unlisteners so no stray onOutput fires into a disposed
      // term. Any subscriptions still in-flight bail via the `disposed` flag.
      // Tearing the listener down on unmount is the OTHER half of the BUG 1 fix:
      // an orphaned terminal://output listener (one whose xterm is gone) is
      // exactly the dead callback id the backend would later emit into, so we
      // must fully drop it rather than leak it.
      if (unlisteners.length > 0) {
        tlog(
          "attach",
          `teardown ${terminalId}: unlistening ${unlisteners.length} channel(s)`,
        );
      }
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

  // Live-apply terminal palette changes (theme editor / MCP set_theme).
  useEffect(() => {
    if (termRef.current) termRef.current.options.theme = toXtermTheme(termPalette);
  }, [termPalette]);

  return <div ref={containerRef} className="termhub-terminal h-full w-full" />;
}
