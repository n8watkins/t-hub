// Terminal drop / paste path-insertion (Lane C).
//
// Two entry points:
//   - C1: installFileDropOnce() wires the WINDOW-level Tauri file-drop event and,
//     on a drop, types the dropped path(s) into whichever tile sat under the
//     cursor (resolved via the existing `data-tile-id` DOM attributes — the same
//     elementFromPoint trick Tile.tsx uses for tile-drag, so we never edit it).
//   - C2: formatPathsForInsert() is reused by Terminal.tsx's Ctrl+V handler to
//     turn a pasted clipboard-image temp path into the same insert string.
//
// Path translation (Windows -> WSL) lives HERE so both flows share one impl: the
// terminals are WSL bash, but dropped paths and the Rust-saved image temp file
// are NATIVE (Windows) paths in the packaged app.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { writeTerminal } from "../ipc/client";
import type { TerminalId } from "../ipc/types";

/**
 * Translate a native Windows path to the WSL mount path the terminals live in:
 * `C:\Users\me\x` -> `/mnt/c/Users/me/x`. Paths that already look POSIX (a
 * leading `/`, e.g. when the app itself runs inside WSL during dev) pass through
 * unchanged, as do UNC/`\\wsl$` style paths we can't sensibly remap.
 */
export function toWslPath(p: string): string {
  // Drive-letter path: `C:\...` or `C:/...`.
  const m = /^([A-Za-z]):[\\/](.*)$/.exec(p);
  if (m) {
    const drive = m[1].toLowerCase();
    const rest = m[2].replace(/\\/g, "/");
    return `/mnt/${drive}/${rest}`;
  }
  // Already POSIX (dev: app runs in WSL) — just normalize separators.
  if (p.startsWith("/")) return p;
  // Anything else (UNC, relative, odd): only swap separators so it's at least
  // a single token; better to insert something the user can fix than nothing.
  return p.replace(/\\/g, "/");
}

/**
 * Quote a path for a POSIX shell only when it needs it (whitespace or shell
 * metacharacters). Uses single quotes, escaping any embedded single quote the
 * standard `'\''` way, so the inserted token is always one safe argument.
 */
export function quoteForShell(p: string): string {
  if (!/[\s"'`$&|;<>()*?!#~\\]/.test(p)) return p;
  return `'${p.replace(/'/g, "'\\''")}'`;
}

/**
 * Turn native drop/paste paths into the text to type at the prompt: translate to
 * WSL, quote each, join with spaces, and add a trailing space so the user can
 * keep typing (or drop again) right after. Empty/blank entries are dropped.
 */
export function formatPathsForInsert(paths: string[]): string {
  const tokens = paths
    .filter((p) => p && p.trim().length > 0)
    .map((p) => quoteForShell(toWslPath(p)));
  if (tokens.length === 0) return "";
  return tokens.join(" ") + " ";
}

/**
 * Resolve which tile (terminal) sits under a viewport point. Mirrors Tile.tsx's
 * dropTargetAt(): elementFromPoint returns the topmost element (usually xterm's
 * canvas), so we walk up to the owning tile via `data-tile-id` (which IS the
 * terminal id). Returns null when the point isn't over any tile.
 */
function terminalAt(x: number, y: number): TerminalId | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  const tileEl = el?.closest<HTMLElement>("[data-tile-id]");
  return tileEl?.getAttribute("data-tile-id") ?? null;
}

let dropInstalled = false;

/**
 * Install the window-level file-drop handler exactly once (idempotent — safe to
 * call from every Terminal mount). Tauri delivers OS file drops as a single
 * window-level event with NATIVE paths and a PHYSICAL-pixel position; we map that
 * position to CSS pixels (÷ devicePixelRatio) to find the tile under it and type
 * the path(s) into that tile's PTY. Never torn down: it's one global listener for
 * the app's lifetime, independent of any single terminal.
 */
export function installFileDropOnce(): void {
  if (dropInstalled) return;
  dropInstalled = true;

  void getCurrentWebview()
    .onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type !== "drop") return;
      const paths = payload.paths;
      if (!paths || paths.length === 0) return;

      // Physical -> CSS pixels: elementFromPoint works in CSS px, and in WebView2
      // devicePixelRatio equals the window scale factor.
      const dpr = window.devicePixelRatio || 1;
      const x = payload.position.x / dpr;
      const y = payload.position.y / dpr;

      const id = terminalAt(x, y);
      if (!id) return; // dropped on chrome/sidebar/empty space — ignore

      const text = formatPathsForInsert(paths);
      if (text) void writeTerminal(id, text);
    })
    .catch(() => {
      // Not running under Tauri (e.g. plain `pnpm dev` in a browser) — no native
      // file-drop event to bind; the feature is simply absent there.
      dropInstalled = false;
    });
}
