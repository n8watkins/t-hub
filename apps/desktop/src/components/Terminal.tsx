// xterm.js terminal tile for the 0.1 terminal nucleus.
//
// RENDERER: xterm 6's built-in DOM renderer is the maintained multi-terminal
// path. The Canvas addon is not supported by xterm 6, and its delayed render
// path appears in the installed lifecycle crash. We also deliberately avoid
// the WebGL addon because each terminal owns a WebGL context and WebView2
// evicts contexts under a dense grid, temporarily blanking terminal tiles.
//
// Responsibilities (PRD §9.1, FR-004/FR-005, §12.1):
//   - Create an xterm.js Terminal with Fit + Search + Unicode11 addons.
//   - On mount/visible: attachTerminal(id, cols, rows), write the base64 scrollback,
//     subscribe onOutput -> xterm.write(decodeBase64(...)).
//   - xterm.onData -> writeTerminal(id, data); ResizeObserver/FitAddon -> resizeTerminal.
//   - Dispose cleanly on unmount. The persistent pool keeps terminals mounted
//     across tab switches; `foreground` only changes output flush cadence.
//
// Lifecycle is keyed on [terminalId, visible]. Warm parked terminals stay
// attached for fast switching and flush output on a slower bounded timer. The
// pool eventually unmounts cold terminals, which runs the complete teardown
// below; a later hot mount subscribes before attach and replays tmux capture.
import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { saneFitProposal } from "./terminalFit";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  attachTerminal,
  closeTerminal,
  decodeBase64,
  listTerminals,
  onExit,
  onOutput,
  resizeTerminal,
  writeTerminal,
} from "../ipc/client";
import { tmuxScroll, tmuxExitScroll, clipboardImageToTemp } from "../ipc/client05";
import type { TerminalId } from "../ipc/types";
import { stripAnsi } from "../lib/ansi";
import { installFileDropOnce, formatPathsForInsert } from "../lib/dropPaste";
import { usePanels } from "../store/panels";
import { useFileOpen } from "../store/fileOpen";
import { useWorkspace } from "../store/workspace";
import { useTheme, DEFAULT_THEME, type TerminalPalette } from "../store/theme";
import { useActivity } from "../store/activity";
import { tlog } from "../lib/diag";
import { REPAINT_ALL_EVENT, REFRESH_TERMINAL_EVENT } from "../lib/repaint";
import { registerTerminalTail, unregisterTerminalTail } from "../lib/terminalTail";
import type { ITheme, ILink } from "@xterm/xterm";
import { clipboardRead, clipboardWrite } from "../lib/clipboard";
import { TerminalCursorBlinkController } from "../lib/terminalCursorBlink";
import { updateTerminalResources } from "../lib/terminalResources";
import { TerminalWriteLifecycle } from "../lib/terminalWriteLifecycle";
import {
  beginTerminalDetach,
  waitForTerminalDetach,
} from "../lib/terminalLifecycle";
import "./Terminal.css";

// Paste into a terminal, preferring an IMAGE on the clipboard over text. When the
// clipboard holds an image (a screenshot, a copied picture), the Rust side saves
// it to a temp PNG and returns that file's native path; we translate it to a WSL
// path and type it into the PTY, because Claude/Codex read images by file path,
// not from a raw bitmap. With no image we fall back to the normal bracketed text
// paste (term.paste), so ordinary copy/paste is completely unchanged.
async function pasteIntoTerminal(
  terminalId: TerminalId,
  term: Terminal,
): Promise<void> {
  // Probe for an image first. Only the PROBE is guarded — once we know there's an
  // image we commit to inserting its path and must NOT fall through to a text
  // paste (a failed image write should not also dump clipboard text into the
  // PTY). A null/throw here means "no image" -> normal text paste.
  let imgPath: string | null = null;
  try {
    imgPath = await clipboardImageToTemp();
  } catch {
    imgPath = null;
  }
  if (imgPath) {
    await writeTerminal(terminalId, formatPathsForInsert([imgPath]));
    return;
  }
  const text = await clipboardRead();
  if (text) term.paste(text);
}

// Hand a URL to the OS default browser. MIRRORS WebPreview.tsx's openExternal():
// the Tauri shell plugin's open() is the primary path (already a JS dependency),
// falling back to window.open only if the native plugin isn't registered. This
// is why the WebLinksAddon below uses a CUSTOM click handler rather than the
// addon's default one — the addon's default is plain window.open, which is
// unreliable in WebView2, so a clicked terminal link could silently do nothing.
async function openExternal(url: string): Promise<void> {
  try {
    await shellOpen(url);
  } catch {
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      /* nothing more we can do from the frontend */
    }
  }
}

// Matches a localhost-style URL printed in terminal output so we can surface it
// as a one-click Preview chip for the tile (Claude/Vite/Next/etc. announce the
// dev server this way). Intentionally narrow — only loopback hosts, with an
// optional :port and path — so we never offer to preview an arbitrary internet
// link the user didn't start. `g` so one chunk can yield several matches.
const LOCALHOST_URL_RE =
  /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0)(?::\d+)?(?:\/[^\s"'<>]*)?/gi;

// How many trailing chars of one output chunk we prepend to the next before
// scanning, so a URL split across two PTY writes is still matched whole. A URL
// here is well under this; the tail just has to outspan the longest plausible
// split point. Kept small — it runs on every output chunk.
const URL_SCAN_TAIL = 256;

// Output scheduling. Foreground terminals flush on rAF for low-latency typing.
// Parked/inactive terminals keep their xterm buffers current, but flush less
// often so inactive workspace tabs, covered panels, minimized windows, and
// maximize/restore transitions do not spend a frame per terminal doing DOM work.
const BACKGROUND_OUTPUT_FLUSH_MS = 250;
const HIDDEN_DOCUMENT_OUTPUT_FLUSH_MS = 1000;
const MAX_BACKGROUND_PENDING_BYTES = 512 * 1024;
// Hard CAP on a parked terminal's pending[] queue (memory). The byte threshold
// above only speeds up the flush; it never bounds the queue, so a hidden tab
// emitting faster than its (throttled) flush drains it grows pending[] without
// limit — a leak plus a huge stall on the eventual flush when the tab is shown.
// xterm scrollback is bounded (20000 lines) anyway, so output older than the most
// recent ~MAX_BACKGROUND_QUEUE_BYTES would be scrolled off the moment it lands; we
// drop those stale OLDEST chunks rather than buffer them. Foreground terminals
// flush every frame and never hit this. Kept a small multiple of the flush
// threshold so a normal background burst is unaffected.
const MAX_BACKGROUND_QUEUE_BYTES = 2 * 1024 * 1024;
const TERMINAL_FOREGROUND_EVENT = "th-terminal-foreground";

// AUTO-REATTACH backoff (attach-loss recovery). An attach stream can drop while
// the tmux session lives on (control-server churn, a server-side detach-client);
// the tile then reads "reconnecting", and a foreground tile retries the attach on
// this schedule until it lands or tmux says the session is really gone. The
// constants mirror the native client's reconnect semantics (apps/native/src/wire:
// BACKOFF_INITIAL 250ms, x2 per attempt, capped at 5s, reset on success).
const RECONNECT_INITIAL_MS = 250;
const RECONNECT_MAX_MS = 5000;

interface TerminalForegroundDetail {
  id: TerminalId;
  foreground: boolean;
}

// Matches a CLICKABLE FILE PATH printed in terminal output (WS-1, open-file-on-
// Ctrl+click). Deliberately STRICT to avoid false positives — we'd rather miss a
// path than underline arbitrary text:
//   - ABSOLUTE paths: POSIX/WSL `/a/b/c` or Windows `C:\a\b` / `C:/a/b`.
//   - RELATIVE paths (WS-1, now that live tile cwd exists): `./x`, `../x`, or a
//     bare `src/app.tsx` shape — resolved against the tile's live cwd in the link
//     provider's activate. The strict shape rules live in looksLikeRelativePath
//     (NOT this regex), which only TOKENIZES; we filter what's openable below.
//   - The final segment must not end in a separator.
// `g` so one line can yield several path matches; segment chars exclude
// whitespace and shell/quote punctuation so we don't swallow surrounding syntax.
// The token may start with a root (`/`, `C:\`), a `./`/`../` prefix, or a bare
// segment — the per-token classifiers (absolute vs. strict-relative) decide
// which are actually surfaced as links.
const FILE_PATH_RE =
  /(?:[A-Za-z]:[\\/]|\.{0,2}[\\/]|(?=[^\s"'`<>|:*?()[\]{}]))(?:[^\s"'`<>|:*?()[\]{}]+[\\/])*[^\s"'`<>|:*?()[\]{}]+/g;

/** True for an ABSOLUTE token: a POSIX `/...` or Windows `C:\...` / `C:/...`. */
function isAbsolutePath(token: string): boolean {
  return /^(?:[A-Za-z]:[\\/]|[\\/])/.test(token);
}

/** True for a token we're willing to open: an absolute path with a real shape —
 *  either it has a directory part (a separator after the root) or a file
 *  extension on its single segment. Trailing sentence punctuation is trimmed by
 *  the caller before this check. */
function looksLikeOpenablePath(token: string): boolean {
  // Drop the leading root marker, then require either an inner separator (a real
  // nested path) or a dotted extension on the leaf so a bare `/usr` doesn't match.
  const afterRoot = token.replace(/^(?:[A-Za-z]:)?[\\/]/, "");
  const hasInnerSep = /[\\/]/.test(afterRoot);
  const leaf = afterRoot.split(/[\\/]/).pop() ?? "";
  const hasExt = /\.[A-Za-z0-9]+$/.test(leaf);
  return hasInnerSep || hasExt;
}

// Source-ish file extensions that make a SINGLE bare segment (no separator)
// openable as a relative path. Kept tight on purpose — a lone `notes.txt` is a
// real reference, but a lone `foo` (no extension) is just a word.
const REL_SINGLE_SEG_EXT_RE =
  /\.(?:ts|tsx|js|jsx|rs|py|go|md|json|toml|txt|sh|css|html|yml|yaml|lock)$/i;

/** STRICT relative-path classifier (WS-1). INTENTIONALLY conservative — lots of
 *  prose reads like `foo/bar`, so we only treat a token as a relative path when
 *  it's clearly one and let everything else fall through as plain text:
 *    1. starts with `./` or `../` (or `.\`/`..\`), OR
 *    2. has at least one `/` AND a final segment with a real file extension
 *       (e.g. `src/app.tsx`, `lib/x.rs`), OR
 *    3. is a SINGLE segment carrying a known source extension (REL_SINGLE_SEG_…).
 *  Bare words, `a/b` without an extension, flags, and URLs (owned by the
 *  WebLinks addon) are deliberately NOT matched. */
function looksLikeRelativePath(token: string): boolean {
  if (isAbsolutePath(token)) return false; // absolute is handled separately
  if (/^\.{1,2}[\\/]/.test(token)) return true; // ./ or ../ prefix
  const leaf = token.split(/[\\/]/).pop() ?? "";
  const hasSep = /[\\/]/.test(token);
  if (hasSep) return /\.[A-Za-z0-9]+$/.test(leaf); // dir + extensioned leaf
  return REL_SINGLE_SEG_EXT_RE.test(token); // lone source-ish file
}

/** POSIX-join a token onto an absolute cwd and normalize `.`/`..` segments — a
 *  pure string op (these are WSL POSIX paths; we never touch node `path`). A
 *  leading `./`/`../` in the token is preserved through normalization, and `..`
 *  pops a parent segment (bounded at the root so it never escapes `/`). Windows
 *  backslashes in the relative token are folded to `/` first since the cwd is
 *  POSIX. Returns an absolute `/...` path. */
function resolveRelativePosix(cwd: string, token: string): string {
  const base = cwd.replace(/\/+$/, ""); // strip trailing slash(es)
  const rel = token.replace(/\\/g, "/");
  const out: string[] = base.split("/").filter((s) => s.length > 0);
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      if (out.length > 0) out.pop();
      continue;
    }
    out.push(seg);
  }
  return "/" + out.join("/");
}

/** Default xterm theme when the active theme carries no terminal palette. */
const DEFAULT_TERM_THEME: ITheme = { background: "#0a0a0a" };

/** Map T-Hub's TerminalPalette onto xterm's ITheme (default when absent). */
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

/**
 * Merge a per-terminal override (a sparse patch set from the tile's ⋯ menu) over
 * the global terminal palette. Returns the base unchanged when there's no
 * override, so terminals without one stay referentially stable (the live-apply
 * effect below won't needlessly re-theme xterm).
 */
function mergeTermPalette(
  base: TerminalPalette | undefined,
  override: Partial<TerminalPalette> | undefined,
): TerminalPalette | undefined {
  if (!override || Object.keys(override).length === 0) return base;
  const b = base ?? DEFAULT_THEME.terminal!;
  return {
    ...b,
    ...override,
    ansi: { ...b.ansi, ...(override.ansi ?? {}) },
  };
}

export interface TerminalViewProps {
  terminalId: TerminalId;
  /** Mount xterm only when visible. The pool usually keeps this true. */
  visible: boolean;
  /**
   * True when this terminal is actually on the active workspace/panel surface.
   * Background terminals stay attached, but output flushing is throttled.
   */
  foreground?: boolean;
}

export function TerminalView({
  terminalId,
  visible,
  foreground = visible,
}: TerminalViewProps): JSX.Element | null {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const cursorBlinkRef = useRef<TerminalCursorBlinkController | null>(null);
  const foregroundRef = useRef(foreground);
  // Guards against a second init for the same (id, visible) effect run even
  // though main.tsx omits StrictMode — belt-and-braces against double `open()`.
  const initializedRef = useRef(false);
  const fitRef = useRef<FitAddon | null>(null);
  // ResizeObserver and zoom effects can run before the async remote PTY attach
  // finishes. Keep their resize commands local until the backend connection is
  // confirmed, otherwise the rejected `resize_terminal` promise becomes a noisy
  // global "no live terminal" error during every restored-tile startup.
  const ptyAttachedRef = useRef(false);
  // Skips the zoom effect's first (mount) run so it doesn't double-fit on open.
  const zoomMountRef = useRef(true);
  // Global zoom: every tile reads the same font size so they scale together.
  const fontSize = useWorkspace((s) => s.fontSize);
  // The focused tile id — when it becomes THIS terminal, we pull keyboard focus
  // into the xterm (see the focus effect below).
  const focusedId = useWorkspace((s) => s.focusedId);
  // Which region navigation targets (feat/keyboard-nav). When it flips back to
  // "terminal" (e.g. Ctrl+B from the sidebar), the focused tile pulls keyboard
  // focus back into its xterm even though `focusedId` itself didn't change.
  const focusedRegion = useWorkspace((s) => s.focusedRegion);
  // Live terminal palette from the active theme (undefined => xterm defaults).
  const termPalette = useTheme((s) => s.active.terminal);
  // This terminal's own color override, if any (set via the tile's ⋯ menu). The
  // selector returns a stable ref unless THIS id's override changes, so other
  // terminals' edits don't re-render us.
  const termOverride = useTheme((s) => s.termOverrides[terminalId]);
  // This terminal's lifecycle state from the store — fed by terminal://state
  // events (Canvas.onState) and the ~15s listTerminals poll. "detached" while
  // this tile is MOUNTED means its tmux session is alive but no PTY streams it:
  // either the attach stream dropped, or the tile never attached at all (seen
  // with tiles materialized via the control-socket create_worktree path). Both
  // are healed by the same reattach sweep below.
  const termState = useWorkspace((s) => s.terminals[terminalId]?.state);
  // Bridge into the init effect's closure: the attach block assigns its reattach
  // trigger here once it exists; before that (or after teardown) it is null and
  // the sweep is a no-op (the initial attach flow owns recovery until then).
  const reattachRef = useRef<(() => void) | null>(null);
  // Buffer resize mutates xterm's line storage and must run behind its async
  // write parser. The zoom effect lives outside the init closure, so it calls
  // through this serialized bridge instead of fitting xterm directly.
  const resizeRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    foregroundRef.current = foreground;
    window.dispatchEvent(
      new CustomEvent<TerminalForegroundDetail>(TERMINAL_FOREGROUND_EVENT, {
        detail: { id: terminalId, foreground },
      }),
    );
  }, [terminalId, foreground]);

  useEffect(() => {
    const container = containerRef.current;
    if (!visible || !container || initializedRef.current) return;
    initializedRef.current = true;

    // Window-level file-drop -> type the dropped path into the tile under the
    // cursor (C1). Idempotent + global: installs on the first terminal mount and
    // lives for the app's lifetime, resolving the target tile itself via
    // data-tile-id, so it's independent of any single terminal instance.
    installFileDropOnce();

    // Disposables collected during async setup; cleanup awaits/runs them all
    // even if the effect is torn down before setup finishes (fast tab flips).
    const unlisteners: UnlistenFn[] = [];
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;
    let promptTimer: ReturnType<typeof setTimeout> | null = null;
    let rafId = 0;
    // Coalesced-write rAF (perf): a flood of small terminal://output events is
    // batched into one frame of decode+write work instead of one synchronous
    // write per event on the single WebView2 JS thread. 0 == none scheduled;
    // torn down in cleanup like `rafId` so it can't fire into a disposed term.
    let flushRaf = 0;
    // Drops bytes that T-Hub has not submitted to xterm yet. A cold unmount can
    // safely discard them because tmux remains authoritative and replays its
    // capture on the next attach.
    let discardPending: (() => void) | null = null;
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    // The reattach loop's pending backoff timer, held at effect scope so cleanup
    // can cancel it — a cleared timer never resolves its sleep, which (with the
    // `disposed` checks) abandons any in-flight reconnect on unmount.
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

    const term = new Terminal({
      allowProposedApi: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, "JetBrains Mono", monospace',
      fontSize: useWorkspace.getState().fontSize,
      // Enabled in place only while this xterm owns visible keyboard focus.
      cursorBlink: false,
      scrollback: 20000,
      // Animate viewport paging (PageUp/PageDown, wheel) over ~125ms instead of
      // jumping, so repeated paging reads as a continuous smooth scroll.
      smoothScrollDuration: 125,
      theme: toXtermTheme(
        mergeTermPalette(
          useTheme.getState().active.terminal,
          useTheme.getState().termOverrides[terminalId],
        ),
      ),
    });
    const writes = new TerminalWriteLifecycle(term);
    updateTerminalResources(terminalId, { xterm: true });
    termRef.current = term;
    // Register this xterm so the captains-deck orchestrator output strip can read
    // its latest visible line on demand (no per-chunk work; the strip polls).
    registerTerminalTail(terminalId, term);

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

    // Clickable web links: underline URLs on hover, open the OS default browser
    // on click. We pass a CUSTOM click handler (not the addon's default, which is
    // plain window.open — unreliable in WebView2) that routes through the Tauri
    // shell plugin via openExternal(); loaded before term.open() like the others.
    const webLinks = new WebLinksAddon((_event: MouseEvent, uri: string) => {
      void openExternal(uri);
    });
    term.loadAddon(webLinks);

    // CLICKABLE FILE PATHS (WS-1): a SECOND link provider, alongside WebLinksAddon
    // (link providers stack — WebLinks underlines URLs, this underlines file
    // paths; the two regexes don't overlap). For the hovered buffer line we read
    // its text and surface each clickable path token as an xterm ILink — both
    // ABSOLUTE paths and STRICT relative paths (the latter resolved against the
    // tile's live cwd on activate; see looksLikeRelativePath, kept intentionally
    // strict to avoid underlining prose). xterm draws the hover underline +
    // pointer cursor for free. We gate the ACTIVATE on Ctrl/Cmd (like VS Code /
    // iTerm "open file"), routing through the fileOpen bus + switching the tile to
    // its Files tab; a plain click is left to xterm (selection), so ordinary
    // drag-to-select copy is unaffected.
    // ONE-ENTRY HOVER CACHE: xterm calls provideLinks for the line under the
    // cursor on effectively every mouse-move across cells, so the same line gets
    // recomputed identically each time the cursor returns to it. We memoize the
    // LAST line only (hover targets one line at a time) keyed on its lineNumber +
    // buffer text; if both match we return the cached ILink[] verbatim instead of
    // re-running the regex scan. The text comparison invalidates naturally when
    // the line's content changes (scroll/reflow/new output) under the same number.
    let cachedLineNumber = -1;
    let cachedText = "";
    let cachedLinks: ILink[] | undefined;
    const pathLinks = term.registerLinkProvider({
      provideLinks: (lineNumber, callback) => {
        const line = term.buffer.active.getLine(lineNumber - 1);
        if (!line) return callback(undefined);
        const text = line.translateToString(false);
        // Serve the memoized result when the hovered line + its text are unchanged
        // (same ILink ranges/activate as a fresh compute — just computed once).
        if (lineNumber === cachedLineNumber && text === cachedText) {
          return callback(cachedLinks);
        }
        // CHEAP PRE-CHECK: a clickable path always carries a `/` (POSIX root,
        // any `dir/leaf`, or a Windows `C:/`) or a `.` (a Windows `:\` drive, or
        // a bare extensioned relative leaf like `app.tsx`). If the line has
        // neither it can't hold an absolute OR a strict-relative path, so we skip
        // the heavy matchAll. Still cache the empty result so a re-hover of that
        // same line doesn't even re-run includes(). (A `.`-only line is rare in
        // practice, so this stays nearly as cheap as the absolute-only check.)
        if (!text.includes("/") && !text.includes(".")) {
          cachedLineNumber = lineNumber;
          cachedText = text;
          cachedLinks = undefined;
          return callback(undefined);
        }
        const links: ILink[] = [];
        FILE_PATH_RE.lastIndex = 0;
        for (const m of text.matchAll(FILE_PATH_RE)) {
          // Trim trailing sentence punctuation a path is unlikely to really end
          // in (".", ",", ")", ":" — e.g. "see /a/b/c.ts:" or "(/a/b)").
          const raw = m[0];
          const token = raw.replace(/[.,:;)\]}]+$/, "");
          // Classify ONCE per token: an absolute path (unchanged shape rules) or
          // a STRICT relative path. The relative matcher is intentionally strict
          // (see looksLikeRelativePath) to avoid underlining prose like `foo/bar`.
          const absolute = isAbsolutePath(token);
          const openable = absolute
            ? looksLikeOpenablePath(token)
            : looksLikeRelativePath(token);
          if (!openable) continue;
          const startX = (m.index ?? 0) + 1; // ILink ranges are 1-based.
          links.push({
            text: token,
            range: {
              start: { x: startX, y: lineNumber },
              end: { x: startX + token.length - 1, y: lineNumber },
            },
            activate: (event, linkText) => {
              // Only Ctrl/Cmd+click opens the file; a bare click falls through to
              // xterm so the user can still place the cursor / start a selection.
              if (!event.ctrlKey && !event.metaKey) return;
              // ABSOLUTE tokens open as-is. RELATIVE tokens resolve against the
              // tile's LIVE cwd (WS-9a: list_terminals refreshes it ~every 5s). If
              // the cwd is missing/empty/non-absolute we can't safely resolve, so
              // we SKIP rather than guess at a wrong path.
              let target = linkText;
              if (!isAbsolutePath(linkText)) {
                const cwd =
                  useWorkspace.getState().terminals[terminalId]?.cwd ?? "";
                if (!cwd.startsWith("/")) return; // no usable cwd -> non-openable
                target = resolveRelativePosix(cwd, linkText);
              }
              usePanels.getState().setTab(terminalId, "files");
              useFileOpen.getState().requestOpen(terminalId, target);
            },
          });
        }
        // Memoize this line's result so a re-hover returns it without recomputing.
        const result = links.length ? links : undefined;
        cachedLineNumber = lineNumber;
        cachedText = text;
        cachedLinks = result;
        callback(result);
      },
    });

    term.open(container);

    // xterm keeps cursor animation alive even when its pooled wrapper is parked.
    // Gate the live option on the helper textarea's actual DOM focus plus the
    // workspace/pool visibility state. This never remounts or resets xterm.
    if (term.textarea) {
      cursorBlinkRef.current = new TerminalCursorBlinkController(
        term.textarea,
        (enabled) => {
          term.options.cursorBlink = enabled;
        },
        {
          visible,
          foreground: foregroundRef.current,
          tileFocused: useWorkspace.getState().focusedId === terminalId,
          terminalRegionFocused:
            useWorkspace.getState().focusedRegion === "terminal",
        },
      );
    }

    // COPY-ON-SELECT (WS-1): mirror Claude Code / iTerm — selecting text in the
    // terminal auto-copies it to the clipboard, no Ctrl+C needed. onSelectionChange
    // fires rapidly during a drag, so we DEBOUNCE (~120ms) and only write once the
    // drag settles. We do NOT clear the selection (it stays highlighted) and this
    // is fully independent of the Ctrl+C handler above (which copies AND clears on
    // demand, and still falls through to SIGINT with no selection).
    let copyTimer: ReturnType<typeof setTimeout> | null = null;
    const selectionSub = term.onSelectionChange(() => {
      if (copyTimer) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        if (disposed || !term.hasSelection()) return;
        const sel = term.getSelection();
        if (sel) void clipboardWrite(sel);
      }, 120);
    });

    // Forward keystrokes/paste to the PTY.
    const dataSub = term.onData((d) => {
      void writeTerminal(terminalId, d);
    });

    // Match the user's Windows Terminal bindings: Ctrl+C copies the selection
    // (and clears it) or, with nothing selected, falls through to the shell as
    // SIGINT; Ctrl+V pastes (bracketed-paste aware via term.paste). Ctrl +/-/0
    // zoom is handled here too because a focused xterm otherwise swallows those
    // before they reach the window-level handler. Returning false stops xterm
    // from sending the key to the PTY; stopPropagation prevents the window
    // handler from double-firing.
    // Tracks whether THIS pane is in tmux copy-mode after a Page Up (set below).
    // A closure flag living for the terminal's lifetime; only ever true after a
    // scroll, so ordinary typing is unaffected while it's false.
    let scrolled = false;
    const sessionName = `th_${terminalId}`;
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;

      // PageUp/PageDown scroll the tmux pane's REAL history via copy-mode — the
      // only way to page back when an alt-screen app (claude/vim) owns the pane
      // (xterm's local scrollback is shallow and the C-b prefix is disabled). We
      // hijack only the BARE keys so Shift+Page* etc. still reach the app. The
      // backend enters copy-mode + pages, and auto-exits at the bottom.
      if (
        (e.key === "PageUp" || e.key === "PageDown") &&
        !e.ctrlKey &&
        !e.altKey &&
        !e.metaKey &&
        !e.shiftKey
      ) {
        scrolled = true;
        void tmuxScroll(sessionName, e.key === "PageDown");
        e.preventDefault();
        e.stopPropagation();
        return false;
      }

      // After a scroll the pane is in copy-mode (output frozen). The first
      // ORDINARY key returns to the live prompt AND is preserved: exit copy-mode,
      // then re-send the character to the PTY so "page up, then just type" works.
      // Arrows / Home / End / Page* stay with copy-mode for native navigation.
      // Guarded by `scrolled`, so normal typing never enters this path.
      if (scrolled) {
        const navKeys = ["PageUp", "PageDown", "ArrowUp", "ArrowDown", "Home", "End"];
        if (navKeys.includes(e.key)) return true;
        scrolled = false;
        const ch =
          e.key === "Enter"
            ? "\r"
            : e.key === "Backspace"
              ? "\x7f"
              : e.key === "Tab"
                ? "\t"
                : e.key.length === 1 && !e.ctrlKey && !e.altKey && !e.metaKey
                  ? e.key
                  : null;
        void tmuxExitScroll(sessionName).then(() => {
          if (ch !== null) void writeTerminal(terminalId, ch);
        });
        e.preventDefault();
        e.stopPropagation();
        return false;
      }

      const mod = e.ctrlKey || e.metaKey;
      if (!mod || e.altKey) return true;
      const key = e.key.toLowerCase();

      if (key === "c") {
        if (term.hasSelection()) {
          void clipboardWrite(term.getSelection());
          term.clearSelection();
          e.preventDefault();
          e.stopPropagation();
          return false;
        }
        return true; // no selection -> let Ctrl+C reach the shell as SIGINT
      }
      if (key === "v") {
        void pasteIntoTerminal(terminalId, term);
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

    const performResize = () => {
      if (disposed || !ptyAttachedRef.current) return;
      // NEVER push a degenerate resize. A parked/hidden/not-yet-laid-out tile
      // measures ~0px wide and FitAddon would propose ~2 cols; pushing that to
      // the PTY wedges the whole tmux window to 2 columns (the 2x24-client bug).
      // Skip until the tile has a real box — the ResizeObserver / pool-move
      // re-runs this once it does, and term keeps its current sane geometry.
      if (!saneFitProposal(term, fit)) return;
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
    const performSettledResize = () => {
      if (disposed || !ptyAttachedRef.current) return;
      if (!saneFitProposal(term, fit)) return;
      performResize();
      // A width change is the only thing that reflows xterm's wrapped scrollback;
      // compare against the LAST settled width (not the pre-fit value) so an
      // unchanged width during a resize burst is a true no-op.
      const widthChanged = term.cols !== lastCols;
      const firstSettle = lastCols === 0;
      lastCols = term.cols;
      // THE BLANK-GRID FIX: term.clear() wipes the ACTIVE buffer. For a
      // full-screen app (Claude Code, vim, less, ...) the active buffer is the
      // ALTERNATE screen, so clearing it erases the app's visible frame — and
      // during a spawn/relayout resize burst it gets erased faster than the app
      // can redraw, so the pane (and, because a spawn reflows every tile, the
      // WHOLE grid) reads blank/muted until something else redraws it. We
      // therefore NEVER clear while the alt buffer is active: full-screen apps
      // repaint themselves on the SIGWINCH from the settled resize, so there is no
      // duplicated scrollback for us to clean up. We also only clear on a REAL
      // width change of an inline (normal-buffer) terminal, and skip the very
      // first settle (nothing to dedupe yet). This collapses the boot/spawn
      // "422 clears" storm to near-zero and removes the blanking entirely.
      const onAltScreen = term.buffer.active.type === "alternate";
      tlog(
        "resize",
        `${terminalId} settle cols->${term.cols} rows=${term.rows} widthChanged=${widthChanged} alt=${onAltScreen}`,
      );
      if (widthChanged && !firstSettle && !onAltScreen) {
        // Defer clear + repaint to the next frame so the post-SIGWINCH redraw of
        // the inline shell lands first; clear only removes scrollback above the
        // live line, then the forced repaint shows a clean, single frame.
        requestAnimationFrame(() => {
          if (disposed) return;
          writes.afterWrites(() => {
            if (disposed) return;
            try {
              term.clear();
            } catch {
              // Renderer/buffer detached mid-call; ignore.
            }
            tlog(
              "resize",
              `${terminalId} cleared scrollback + repaint (rows=${term.rows})`,
            );
            forceFullRedraw();
          });
        });
      } else {
        forceFullRedraw();
      }
    };
    const settleResize = () => {
      // xterm parses write() input asynchronously. Resizing its buffer while a
      // line-feed parser action is in flight can leave the next row absent and
      // throw from InputHandler.isWrapped. Keep one latest resize queued behind
      // all accepted writes; repeated observer/zoom events coalesce in place.
      writes.afterWritesCoalesced("resize", performSettledResize);
    };
    resizeRef.current = settleResize;

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
          // Only fit if the tile is really laid out at a sane size. A tile that
          // spawns in the BACKGROUND (another tab active) has no measured box
          // yet, so a fit here would size xterm to ~2 cols and the attach below
          // would create the PTY at 2 cols — the wedged 2x24 client. Skipping
          // leaves xterm at its 80x24 construction default, a sane geometry to
          // attach at; the pool-move / ResizeObserver re-fits to the tile's real
          // width the moment it gets a box.
          if (saneFitProposal(term, fit)) fit.fit();
        } catch {
          /* container detached; ignore */
        }
        void (async () => {
          // Recovery hook for a FAILED initial attach, assigned inside the try
          // (where the reattach helpers live — `try` block scope hides them from
          // the catch). Null only if the failure precedes the helpers' setup, in
          // which case there is nothing to recover with yet.
          let recoverInitialAttach: ((err: unknown) => Promise<void>) | null =
            null;
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

            // LOCALHOST-URL DETECTION: scan the LIVE PTY stream for dev-server
            // URLs and publish each NEW one to the panels store, where the tile's
            // Preview tab renders them as one-click chips. Decoupled from xterm's
            // write path so it can't perturb rendering. A streaming TextDecoder
            // carries multi-byte UTF-8 across chunk boundaries; a rolling tail of
            // the previous chunk is prepended so a URL split across two writes is
            // still matched whole. We dedupe against the last few URLs we pushed
            // so a server logging its URL on every request doesn't spam the store
            // (addDetectedUrl also dedupes, but this avoids the regex+set churn).
            const urlDecoder = new TextDecoder("utf-8");
            let scanTail = "";
            const recentUrls: string[] = [];
            const scanForUrls = (bytes: Uint8Array): void => {
              // `stream: true` keeps a trailing partial code point for next time.
              const raw = scanTail + urlDecoder.decode(bytes, { stream: true });
              // Strip ANSI/VT escapes BEFORE matching: the raw pty stream
              // interleaves color/cursor/erase codes with the text, and without
              // this they get captured into the URL (e.g. ".../preview\x1b[K\x1b[m
              // \x1b[28;1H"). Match on the cleaned text; carry the RAW tail so an
              // escape — or a URL — split across two writes still resolves on the
              // next chunk.
              const text = stripAnsi(raw);
              LOCALHOST_URL_RE.lastIndex = 0;
              for (const m of text.matchAll(LOCALHOST_URL_RE)) {
                const url = m[0];
                if (recentUrls.includes(url)) continue;
                recentUrls.push(url);
                if (recentUrls.length > 16) recentUrls.shift();
                usePanels.getState().addDetectedUrl(terminalId, url);
              }
              // Carry the tail so a URL spanning this chunk and the next is caught.
              scanTail =
                raw.length > URL_SCAN_TAIL ? raw.slice(-URL_SCAN_TAIL) : raw;
            };

            // PER-TERMINAL COALESCED WRITE (perf): the old path decoded, scanned
            // for URLs (stripAnsi + regex over a rolling buffer), and wrote to
            // xterm SYNCHRONOUSLY for every single output event, on the one
            // WebView2 JS thread, for every live terminal. A burst from a few
            // busy terminals turned into a decode/ANSI/regex storm that froze the
            // app. Now `onOutput` only decodes and ENQUEUES the bytes; a single
            // rAF flush per terminal does the writing + (bounded) URL scan once
            // per frame, collapsing a burst of small events into one frame of
            // work. Byte ORDER is preserved because the queue is FIFO and a flush
            // drains it in arrival order. T-Hub's not-yet-submitted queue is
            // discarded on cold teardown because tmux replays authoritative
            // output on the next attach.
            const pending: Uint8Array[] = [];
            let pendingBytes = 0;

            // Drain the queued bytes: URL-scan + activity-bump + write, ONCE for
            // the whole frame's worth of chunks.
            const drainQueue = (): void => {
              if (pending.length === 0) return;
              // Snapshot + clear up front so anything that arrives DURING this
              // drain queues cleanly for the next frame (FIFO order intact).
              const chunks = pending.splice(0, pending.length);
              pendingBytes = 0;

              // URL detection runs at most ONCE per flush over the frame's bytes
              // (in arrival order), not once per chunk — same stripAnsi/regex
              // work, far fewer invocations. The dedup ring + tail carry-over
              // make this identical in effect to the old per-chunk scan: a URL
              // split across chunks within or across frames still resolves via
              // `scanTail`, and `recentUrls` still suppresses repeats.
              for (const bytes of chunks) scanForUrls(bytes);

              // RUNNING signal (#11): bump ONCE per flush rather than per chunk.
              // The sidebar pulse is a coarse "output is flowing" indicator, so a
              // per-frame bump is indistinguishable from a per-chunk one to the
              // user while saving a store write per event.
              useActivity.getState().bump(terminalId);

              // Write the frame's bytes to xterm in arrival order. Before the
              // seed has landed we still route into `liveBuffer` (flushed after
              // the seed) so history/live ordering is preserved exactly as before.
              if (seeded) {
                for (const bytes of chunks) writes.write(bytes);
              } else {
                for (const bytes of chunks) liveBuffer.push(bytes);
              }
            };

            const flushPending = (): void => {
              flushRaf = 0;
              if (disposed) return;
              drainQueue();
            };
            const flushPendingTimer = (): void => {
              flushTimer = null;
              if (disposed) return;
              drainQueue();
            };

            const documentHidden = (): boolean =>
              typeof document !== "undefined" && document.visibilityState === "hidden";
            const shouldFlushForeground = (): boolean =>
              foregroundRef.current && !documentHidden();

            const scheduleFlush = (): void => {
              if (pending.length === 0) return;
              if (shouldFlushForeground()) {
                if (flushTimer) {
                  clearTimeout(flushTimer);
                  flushTimer = null;
                }
                if (flushRaf === 0) {
                  flushRaf = requestAnimationFrame(flushPending);
                }
                return;
              }

              // CAP the parked queue (memory): if output is arriving on a hidden
              // tab faster than the throttled flush drains it, pending[] would grow
              // unbounded. Drop the OLDEST chunks until we're back under the cap —
              // anything that far back would be scrolled out of xterm's bounded
              // scrollback before the tab is ever shown, so dropping it is safe and
              // avoids both the leak and a giant single flush on reveal. FIFO order
              // of the surviving (most-recent) chunks is preserved.
              //
              // Chunks are raw PTY byte slices, so a naive cut can leave the SURVIVOR
              // starting mid-UTF-8-codepoint or mid-ANSI-escape, which xterm's
              // streaming parser would render as garbage on reveal. So once we've
              // dropped at all, keep dropping until the LAST dropped chunk ended on a
              // newline (0x0a) — the survivor then begins at a clean line boundary.
              let dropped = false;
              let lastEndedNewline = true;
              while (
                pending.length > 1 &&
                (pendingBytes > MAX_BACKGROUND_QUEUE_BYTES ||
                  (dropped && !lastEndedNewline))
              ) {
                const chunk = pending.shift();
                if (!chunk) break;
                pendingBytes -= chunk.byteLength;
                dropped = true;
                lastEndedNewline =
                  chunk.byteLength > 0 && chunk[chunk.byteLength - 1] === 0x0a;
              }
              if (flushRaf !== 0 || flushTimer) return;
              const delay =
                pendingBytes >= MAX_BACKGROUND_PENDING_BYTES
                  ? 0
                  : documentHidden()
                    ? HIDDEN_DOCUMENT_OUTPUT_FLUSH_MS
                    : BACKGROUND_OUTPUT_FLUSH_MS;
              flushTimer = setTimeout(flushPendingTimer, delay);
            };

            // -----------------------------------------------------------------
            // ATTACH-LOSS RECOVERY: verify-before-exit + auto-reattach.
            //
            // An `exit` event or a dropped attach stream does NOT prove the
            // process exited — server-side attach churn closes the stream while
            // the tmux session keeps running (the false "[process exited]" over a
            // live session). So: (1) never declare exited on the stream ending
            // alone — verify liveness against the session list first; (2) if the
            // session is alive but unattached, reattach with capped backoff
            // (native-client semantics: 250ms, x2, 5s cap). Only a FOREGROUND
            // tile retries; a parked/background one just marks itself and resumes
            // the moment it is foregrounded, so we never fight the pool's
            // deliberate handling of hidden tiles. A deliberate close/kill
            // unmounts this component (`disposed`) and removes the store record,
            // both of which stop the loop.
            // -----------------------------------------------------------------
            let reconnecting = false;
            let needsReattach = false;
            let initialAttachSettled = false;

            const reconnectSleep = (ms: number): Promise<void> =>
              new Promise((resolve) => {
                reconnectTimer = setTimeout(() => {
                  reconnectTimer = null;
                  resolve();
                }, ms);
              });

            // Is this terminal's tmux session still alive? Asks the backend for
            // the reconciled session list (the same tmux walk the sidebar poll
            // uses). On a FAILED probe err on the side of "alive": a liveness
            // check that can't run must never paint a false [process exited] —
            // the reattach loop keeps verifying on every retry anyway.
            const sessionAlive = async (): Promise<boolean> => {
              try {
                const list = await listTerminals();
                return list.some((t) => t.id === terminalId);
              } catch {
                return true;
              }
            };

            // The VERIFIED death path — the only place the exited banner renders.
            const declareExited = (): void => {
              // Flush queued output first so the process's final bytes land
              // before the banner (same ordering the old exit handler kept).
              drainQueue();
              writes.write("\r\n[process exited]\r\n");
              useWorkspace.getState().updateState(terminalId, "exited");
            };

            // Retry the attach until it lands, the session turns out dead, or the
            // tile goes away. Reuses the normal attach + scrollback-seed path: on
            // success the grid is reset and repopulated from the fresh seed, so
            // the pane picks up exactly where the session is (no duplicated
            // history from the pre-drop buffer).
            const reattachLoop = async (): Promise<void> => {
              if (disposed || reconnecting || !initialAttachSettled) return;
              reconnecting = true;
              needsReattach = false;
              writes.write("\r\n[session detached - reconnecting...]\r\n");
              let delay = RECONNECT_INITIAL_MS;
              for (;;) {
                await reconnectSleep(delay);
                if (disposed) return;
                // Tile removed from the workspace while we waited → deliberate
                // close/kill in flight; stand down.
                if (!useWorkspace.getState().terminals[terminalId]) {
                  reconnecting = false;
                  return;
                }
                // Backgrounded: park the retry; onForegroundChanged resumes it.
                if (!foregroundRef.current) {
                  reconnecting = false;
                  needsReattach = true;
                  return;
                }
                if (!(await sessionAlive())) {
                  if (!disposed) declareExited();
                  reconnecting = false;
                  return;
                }
                if (disposed) return;
                try {
                  // Evict any DEAD RemotePty conn for this id first: after a
                  // drop the backend map still holds it, and attach_terminal
                  // would see "already streaming" and return an empty seed
                  // without opening a new socket. close_terminal detaches +
                  // removes it (a no-op when absent); the tmux session survives.
                  ptyAttachedRef.current = false;
                  await closeTerminal(terminalId);
                  updateTerminalResources(terminalId, { pty: false });
                  if (disposed) return;
                  // Buffer live output while we await the fresh seed, exactly
                  // like the mount path: bytes from the new conn replay AFTER
                  // the seed. Pre-drop queue contents are superseded by the
                  // seed, so drop them.
                  seeded = false;
                  pending.length = 0;
                  pendingBytes = 0;
                  liveBuffer.length = 0;
                  const scrollback = await attachTerminal(
                    terminalId,
                    term.cols,
                    term.rows,
                  );
                  if (disposed) return;
                  ptyAttachedRef.current = true;
                  updateTerminalResources(terminalId, { pty: true });
                  // Repopulate: reset the grid so the stale pre-drop frame (and
                  // the reconnecting banner) never duplicates under the seed.
                  await writes.waitForWrites();
                  if (disposed) return;
                  term.reset();
                  const seed = decodeBase64(scrollback);
                  if (seed.length > 0) writes.write(seed);
                  seeded = true;
                  if (liveBuffer.length > 0) {
                    for (const chunk of liveBuffer) writes.write(chunk);
                    liveBuffer.length = 0;
                  }
                  // An empty capture renders nothing — nudge the shell to
                  // repaint its prompt, exactly like the fresh-spawn path.
                  if (seed.length === 0) void writeTerminal(terminalId, "\x0c");
                  reconnecting = false;
                  tlog(
                    "attach",
                    `reattached ${terminalId} after attach loss (seed ${seed.length}B)`,
                  );
                  return;
                } catch (err) {
                  // Attach refused (session raced away, control socket down,…):
                  // restore live-write mode and back off. The next iteration
                  // re-verifies liveness, so a genuinely-dead session converges
                  // to the exited banner instead of retrying forever.
                  seeded = true;
                  tlog(
                    "attach",
                    `reattach ${terminalId} failed (${String(err)}); retrying in ${delay}ms`,
                  );
                  delay = Math.min(delay * 2, RECONNECT_MAX_MS);
                }
              }
            };

            // Expose the trigger to the store-state sweep effect (a mounted tile
            // whose store state reads "detached" — dropped OR never attached —
            // schedules a reattach through this).
            reattachRef.current = () => void reattachLoop();

            // And to the catch below (block scoping hides these helpers there):
            // a failed INITIAL attach recovers exactly like a dropped one.
            recoverInitialAttach = async (err: unknown): Promise<void> => {
              initialAttachSettled = true;
              tlog(
                "attach",
                `initial attach ${terminalId} failed: ${String(err)}`,
              );
              if (disposed) return;
              if (await sessionAlive()) {
                if (!disposed) void reattachLoop();
              } else if (!disposed) {
                declareExited();
              }
            };

            const onForegroundChanged = (
              event: Event,
            ): void => {
              const detail = (event as CustomEvent<TerminalForegroundDetail>).detail;
              if (detail?.id !== terminalId) return;
              if (detail.foreground) {
                scheduleFlush();
                // A drop noticed while parked resumes its reattach here, the
                // moment the tile is foregrounded again.
                if (needsReattach) void reattachLoop();
              }
            };
            const onVisibilityChanged = (): void => {
              if (documentHidden()) {
                if (flushRaf !== 0) {
                  cancelAnimationFrame(flushRaf);
                  flushRaf = 0;
                }
                scheduleFlush();
              } else {
                scheduleFlush();
              }
            };
            window.addEventListener(TERMINAL_FOREGROUND_EVENT, onForegroundChanged);
            document.addEventListener("visibilitychange", onVisibilityChanged);

            discardPending = () => {
              pending.length = 0;
              pendingBytes = 0;
              liveBuffer.length = 0;
            };

            const offOutput = await onOutput(terminalId, (e) => {
              if (disposed) return;
              // HOT PATH: decode + enqueue only. The heavy work (URL scan, store
              // bump, term.write) is deferred to the coalesced rAF flush above so
              // a flood of events doesn't block the JS thread chunk-by-chunk.
              const bytes = decodeBase64(e.base64);
              pending.push(bytes);
              pendingBytes += bytes.byteLength;
              scheduleFlush();
            });
            if (disposed) {
              window.removeEventListener(
                TERMINAL_FOREGROUND_EVENT,
                onForegroundChanged,
              );
              document.removeEventListener("visibilitychange", onVisibilityChanged);
              void offOutput();
              return;
            }
            unlisteners.push(offOutput);
            unlisteners.push(() => {
              window.removeEventListener(
                TERMINAL_FOREGROUND_EVENT,
                onForegroundChanged,
              );
              document.removeEventListener("visibilitychange", onVisibilityChanged);
            });

            const offExit = await onExit(terminalId, (e) => {
              if (disposed) return;
              // VERIFY BEFORE EXIT: an exit event alone doesn't prove the
              // process died — the server-side attach client also exits on a
              // detach while the tmux session lives on. (The backend now
              // verifies too and emits Detached instead, but the tile must
              // never render death on attach loss even if an unverified emit
              // slips through.) Session alive → treat as attach loss and
              // reattach; gone → the real exited banner.
              void (async () => {
                const alive = await sessionAlive();
                if (disposed) return;
                if (alive) void reattachLoop();
                else declareExited();
              })();
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

            await waitForTerminalDetach(terminalId);
            if (disposed) return;
            const scrollback = await attachTerminal(
              terminalId,
              term.cols,
              term.rows,
            );
            if (disposed) return;
            ptyAttachedRef.current = true;
            updateTerminalResources(terminalId, { pty: true });
            // Empty seed => fresh spawn (backend skips capture); non-empty =>
            // reattach history to restore. Only write a real seed; a fresh
            // prompt is drawn by the forced redraw below.
            const seed = decodeBase64(scrollback);
            const freshSpawn = seed.length === 0;
            if (!freshSpawn) writes.write(seed);

            // Seed is on screen; switch to live writes and flush anything that
            // arrived on the listener while we were awaiting attach/seed.
            seeded = true;
            if (liveBuffer.length > 0) {
              for (const chunk of liveBuffer) writes.write(chunk);
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
            initialAttachSettled = true;
          } catch (err) {
            // Initial attach failed. Previously this left the tile rendered but
            // INERT forever — the "tile never attached" signature (seen with
            // control-socket create_worktree spawns). Recover exactly like a
            // dropped attach: session alive → reattach with backoff; session
            // really gone → the verified exited banner. (Null hook = the failure
            // preceded the recovery setup; nothing to retry with.)
            await recoverInitialAttach?.(err);
          }
        })();
      });
    });

    // Force xterm's renderer to repaint the whole viewport. Every renderer (the
    // active CANVAS one, and the DOM/GPU ones before it) skips redrawing a frame
    // when the element merely goes hidden->visible (or the window settles after a
    // resize) at the SAME size, because nothing wrote new cells and no
    // fit/SIGWINCH fired -- so
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
          `[t-hub] forceRepaint on terminal ${terminalId} with no buffer (rows=${t.rows})`,
        );
        return;
      }
      try {
        t.refresh(0, t.rows - 1);
      } catch {
        // Renderer detached mid-call; ignore.
      }
    };

    // The RELIABLE recomposite — the programmatic equivalent of "scroll/type
    // heals the frozen frame". A bare t.refresh() only marks rows dirty; the
    // 2D-canvas renderer then skips redrawing "unchanged" cells and keeps the
    // CACHED geometry, so a stale frame after a geometry change (new session
    // relayout, maximize/restore, drag) survives — and the ⟳ reload button, which
    // only fit()s+refreshes, can't clear it when cols/rows didn't change (fit is
    // then a no-op). term.clearTextureAtlas() throws away the cached glyph atlas
    // and forces a full re-composite, exactly what a scroll does. It RE-RASTERIZES
    // glyphs (heavier than refresh), so we self-throttle the atlas clear to at most
    // ~10x/sec: a discrete heal (maximize/restore/new-session) clears immediately;
    // a continuous drag that calls this per frame only clears every 100ms and falls
    // back to a plain refresh in between — bounded cost, never a per-frame atlas
    // rebuild. (fit() — the only thing that fixes a SIZE change — stays on the
    // debounced settle; this just re-composites correctly into the resized canvas.)
    let lastAtlasClearMs = 0;
    const forceFullRedraw = () => {
      if (disposed) return;
      const t = termRef.current;
      if (!t || t.rows <= 0) return;
      const now =
        typeof performance !== "undefined" ? performance.now() : Date.now();
      if (now - lastAtlasClearMs >= 100) {
        lastAtlasClearMs = now;
        try {
          t.clearTextureAtlas();
        } catch {
          // DOM-renderer fallback has no atlas; ignore.
        }
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
    // becomes the visible active tab again -- the moment its canvas would
    // otherwise read a stale/blank frame. We repaint on the NEXT frame so it lands after the
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

    // Re-FIT (not just repaint) on an on-screen REPOSITION-OR-RESIZE. The pool
    // dispatches a "th-pool-moved" event on this terminal's wrapper whenever it
    // repositions OR RESIZES a VISIBLE terminal (a same-tab reorder/swap, OR a
    // GROW from a small corner tile to large/full — see TerminalPool.applyVisible,
    // which now fires on a size change too, not only a transform change).
    //
    // ROOT CAUSE this fixes: when the pool grows a tile by writing a new
    // width/height onto the wrapper imperatively (inside its layout-effect/rAF),
    // the inner container's ResizeObserver tick can be missed/coalesced, so the
    // 250ms settle that runs fit.fit()+resizeTerminal never fires for the grow —
    // the buffer stays wrapped at the OLD narrow width (no new cols => no reflow).
    // The pool-move signal, by contrast, ALWAYS fires on a grow, so we drive a
    // real re-fit from it. We route through the SAME debounced settleResize the
    // ResizeObserver uses (not pushResize directly), so this reuses the 250ms
    // anti-burst coalescing, the width-change dedupe (no reflow when cols are
    // unchanged — so we never double-fit the normal grid-resize path that already
    // worked), and the alt-screen guard (never term.clear() the live frame of a
    // full-screen app mid-resize). settleResize already repaints, so the
    // stale-frame repaint this used to do is preserved.
    const wrapEl = container.closest("[data-th-pool-tile]") as HTMLElement | null;
    const onPoolMoved = () => {
      // LEADING heal: a new-session relayout / grow resizes this tile but fires NO
      // window onResized and NO focus, so repaintMount never runs — without this the
      // foreground tile shows a stale/frozen frame for the whole 250ms debounce
      // (the "open a new session blanks a terminal" case). Recomposite the
      // foreground tile next frame; the trailing settle still does the real
      // fit+reflow-dedupe. (We do NOT fit per call — the fit stays on the settle so
      // a continuous drag still coalesces to one SIGWINCH.)
      if (foregroundRef.current) requestAnimationFrame(forceFullRedraw);
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(settleResize, 250);
    };
    wrapEl?.addEventListener("th-pool-moved", onPoolMoved);

    // Repaint when ANY full-screen overlay (spawn-preset menu, file/web preview,
    // Settings) opens or closes. WebView2 leaves the terminals on a
    // stale/blank frame when a new `fixed` layer is added over them or removed,
    // and nothing moves or resizes — so the two repaint paths above never fire.
    // repaintAllTerminals() broadcasts on every overlay toggle; we redraw on the
    // next frame, after that overlay change has painted. See src/lib/repaint.ts.
    // FOREGROUND-ONLY: only the on-surface terminals can be left muted by an
    // overlay appearing/disappearing over them, and only those are worth a frame of
    // redraw on the toggle. Background/hidden terminals are parked offscreen, so an
    // overlay never covers them; they self-heal via the IntersectionObserver repaint
    // when brought back to the foreground. Skipping them keeps an overlay toggle
    // from doing a full repaint of every pooled terminal.
    const onRepaintAll = () => {
      if (!foregroundRef.current) return;
      // forceFullRedraw (not just refresh): a same-size restore-from-minimize or an
      // overlay close needs the canvas RE-COMPOSITED, which a bare refresh won't do
      // (see forceFullRedraw). The atlas-clear self-throttles, so a continuous
      // window drag (repaintMount's per-frame leading rAF lands here too) only
      // rebuilds the atlas ~10x/sec, not every frame.
      requestAnimationFrame(forceFullRedraw);
    };
    window.addEventListener(REPAINT_ALL_EVENT, onRepaintAll);

    // Manual per-terminal refresh (tile header ⟳ / right-click): RE-FIT to the
    // current container size — pushing fresh cols/rows to the PTY so the shell
    // reflows — then repaint. The recovery for a tile grown from a small corner to
    // full that didn't reflow on its own. Targets this terminal by id; an
    // undefined id refreshes all (parity with the repaint-all broadcast).
    const onRefresh = (e: Event) => {
      const eid = (e as CustomEvent<{ id?: string }>).detail?.id;
      if (eid && eid !== terminalId) return;
      // A no-id BROADCAST (repaintMount's window-resize/maximize settle) would
      // otherwise resize EVERY pooled terminal - ~16 synchronous fit.fit()
      // calls = the observed ~327ms main-thread stall. Background tiles re-fit on
      // un-park via the IntersectionObserver, so for a broadcast only fit+heal the
      // FOREGROUND tile. A TARGETED refresh (eid set — the ⟳ reload button on this
      // tile) always runs regardless of foreground.
      if (!eid && !foregroundRef.current) return;
      // Re-fit + RECOMPOSITE. We do NOT reset/re-seed xterm's buffer here: doing
      // that on a LIVE terminal (especially a full-screen app in the alt-screen
      // buffer) mangles the formatting. Deep scroll-up is handled by Page Up (tmux
      // copy-mode), which reads the real tmux history (up to history-limit).
      // forceFullRedraw (not refresh) is why the ⟳ button now actually clears a
      // stale frame: when cols/rows are unchanged, the fit is a no-op,
      // so only the atlas-clear re-composite heals it.
      settleResize();
      requestAnimationFrame(forceFullRedraw);
    };
    window.addEventListener(REFRESH_TERMINAL_EVENT, onRefresh);

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
      // LEADING heal so a grid-GUTTER drag (resizing tiles inside the window — no
      // window onResized, no pool-move; only this observer fires) doesn't sit on a
      // frozen frame for the 250ms debounce. forceFullRedraw self-throttles its
      // atlas clear, so per-callback during a continuous drag is bounded; the FIT
      // stays on the trailing settle ONLY (no per-frame SIGWINCH/reflow storm).
      if (foregroundRef.current) requestAnimationFrame(forceFullRedraw);
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(settleResize, 250);
    });
    resizeObserver.observe(container);

    return () => {
      disposed = true;
      initializedRef.current = false;

      cancelAnimationFrame(rafId);
      // Cancel any scheduled coalesced-write flush. The unsubmitted queue is
      // discarded below; tmux replays it from authoritative capture on attach.
      if (flushRaf !== 0) {
        cancelAnimationFrame(flushRaf);
        flushRaf = 0;
      }
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      if (promptTimer) clearTimeout(promptTimer);
      if (resizeTimer) clearTimeout(resizeTimer);
      if (copyTimer) clearTimeout(copyTimer);
      // Abandon any in-flight reattach: the cleared timer never resolves its
      // sleep, and the loop's `disposed` checks bail on every other await.
      reattachRef.current = null;
      if (resizeRef.current === settleResize) resizeRef.current = null;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      resizeObserver?.disconnect();
      resizeObserver = null;
      visObserver?.disconnect();
      visObserver = null;
      wrapEl?.removeEventListener("th-pool-moved", onPoolMoved);
      window.removeEventListener(REPAINT_ALL_EVENT, onRepaintAll);
      window.removeEventListener(REFRESH_TERMINAL_EVENT, onRefresh);

      dataSub.dispose();
      selectionSub.dispose();
      pathLinks.dispose();

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

      discardPending?.();
      ptyAttachedRef.current = false;

      cursorBlinkRef.current?.dispose();
      cursorBlinkRef.current = null;

      unregisterTerminalTail(terminalId, term);
      // xterm parses accepted writes asynchronously. Retire immediately so no
      // later write is accepted, then dispose only after parser callbacks settle.
      writes.disposeWhenIdle();
      void beginTerminalDetach(terminalId, () => closeTerminal(terminalId)).catch(
        () => undefined,
      );
      updateTerminalResources(terminalId, {
        xterm: false,
        canvas: false,
        pty: false,
      });
      termRef.current = null;
      fitRef.current = null;
    };
  }, [terminalId, visible]);

  // REATTACH SWEEP: a mounted, visible tile whose store state reads "detached"
  // has a live tmux session with no PTY streaming it — the attach dropped (the
  // backend's verified stream-end emits Detached), or the tile never attached in
  // the first place (the ~15s listTerminals poll flips it to detached). Either
  // way, schedule a reattach. The trigger is a no-op until the init effect's
  // attach block has installed it (the initial attach owns recovery until then),
  // and the loop itself re-checks foreground/liveness before doing anything, so
  // this can never fight a deliberate close (which unmounts and clears the ref).
  useEffect(() => {
    if (!visible || termState !== "detached") return;
    reattachRef.current?.();
  }, [termState, visible, terminalId]);

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
    resizeRef.current?.();
  }, [fontSize, terminalId]);

  // Live-apply terminal palette changes — global (theme editor / MCP set_theme)
  // or this terminal's own ⋯-menu override — without recreating the terminal.
  useEffect(() => {
    if (termRef.current)
      termRef.current.options.theme = toXtermTheme(
        mergeTermPalette(termPalette, termOverride),
      );
  }, [termPalette, termOverride]);

  // Update cursor animation eligibility without touching xterm's mount or
  // attachment. `foreground` covers active-tab, expanded-panel, fullscreen, and
  // captain-overlay parking; the controller separately verifies actual input,
  // window, and document focus.
  useEffect(() => {
    cursorBlinkRef.current?.update({
      visible,
      foreground,
      tileFocused: focusedId === terminalId,
      terminalRegionFocused: focusedRegion === "terminal",
    });
  }, [visible, foreground, focusedId, terminalId, focusedRegion]);

  // When this terminal becomes the focused tile, put the KEYBOARD into its xterm
  // (not just the tile highlight) so the user can type right away — e.g. the
  // instant a Recall/spawn lands and `claude --resume` shows its picker, no extra
  // click to focus the terminal. rAF defers until after the pool positions/shows
  // the tile this frame; guarded by `visible` + a live term instance. Also fires
  // on mount (first effect run) so a freshly-spawned focused tile grabs focus.
  useEffect(() => {
    if (!visible || focusedId !== terminalId) return;
    // Only steal focus when navigation is on the terminal region — when the
    // sidebar region is focused (Ctrl+B), the user is keyboard-driving the
    // sidebar and this effect must not yank focus back into the xterm. Returning
    // to "terminal" re-runs this effect (focusedRegion is a dep) and refocuses.
    if (focusedRegion !== "terminal") return;
    const raf = requestAnimationFrame(() => {
      try {
        termRef.current?.focus();
      } catch {
        /* container detached mid-focus; ignore */
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [focusedId, terminalId, visible, focusedRegion]);

  // No custom right-click menu (per request). preventDefault only suppresses the
  // WebView's own context menu; tmux's mouse menu (split/kill/respawn/mark/zoom)
  // is disabled server-side in tmux.rs (unbind MouseDown3Pane). Copy = Shift+drag
  // to select, then Ctrl+C (clipboard plugin); Ctrl+V pastes.
  return (
    <div
      ref={containerRef}
      className="t-hub-terminal h-full w-full"
      onContextMenu={(e) => e.preventDefault()}
    />
  );
}
