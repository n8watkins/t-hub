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
 * `C:\Users\me\x` -> `/mnt/c/Users/me/x`, and a WSL UNC path
 * `\\wsl$\Ubuntu\home\me\x` -> `/home/me/x`. Paths that already look POSIX (a
 * leading `/`, e.g. when the app itself runs inside WSL during dev) pass through
 * unchanged; anything else we can't classify gets separators normalized only.
 */
export function toWslPath(p: string): string {
  // WSL UNC path (dragging a file FROM a distro folder shown in Explorer):
  // `\\wsl$\Ubuntu\home\me\f` or `\\wsl.localhost\Ubuntu\home\me\f`. The segment
  // after the prefix is the DISTRO name; everything past it is already the rootfs
  // POSIX path, so drop the prefix + distro and keep the rest.
  const unc = /^\\\\wsl(?:\$|\.localhost)\\[^\\]+\\?(.*)$/i.exec(p);
  if (unc) return "/" + unc[1].replace(/\\/g, "/");
  // Drive-letter path: `C:\...` or `C:/...`.
  const m = /^([A-Za-z]):[\\/](.*)$/.exec(p);
  if (m) {
    const drive = m[1].toLowerCase();
    const rest = m[2].replace(/\\/g, "/");
    return `/mnt/${drive}/${rest}`;
  }
  // Already POSIX (dev: app runs in WSL) — just normalize separators.
  if (p.startsWith("/")) return p;
  // Anything else (other UNC, relative, odd): only swap separators so it's at
  // least a single token; better to insert something the user can fix than
  // nothing.
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

// Control characters (incl. CR/LF) are stripped from every path before it's
// typed. The token goes RAW into a live PTY, where a literal newline is Enter —
// single-quoting protects the shell PARSER but not the line discipline, so a file
// named `foo\nrm -rf ~` would otherwise submit a truncated/extra command. Real
// filenames don't contain control chars, so removing them only neutralizes the
// hostile/degenerate case.
// eslint-disable-next-line no-control-regex
const CONTROL_CHARS = /[\x00-\x1f\x7f]/g;

/**
 * Turn native drop/paste paths into the text to type at the prompt: translate to
 * WSL, strip control chars, quote each, join with spaces, and add a trailing
 * space so the user can keep typing (or drop again) right after. Empty/blank
 * entries are dropped.
 */
export function formatPathsForInsert(paths: string[]): string {
  const tokens = paths
    .map((p) => toWslPath(p).replace(CONTROL_CHARS, ""))
    .filter((p) => p.trim().length > 0)
    .map((p) => quoteForShell(p));
  if (tokens.length === 0) return "";
  return tokens.join(" ") + " ";
}

/**
 * Resolve which terminal sits under a viewport point. elementFromPoint returns
 * the topmost element, then we walk up to the owning tile. We accept TWO anchors
 * because the xterm body and the tile chrome live in different DOM subtrees:
 *
 *   - `data-th-pool-tile` — the persistent-pool wrapper that actually holds the
 *     xterm canvas (TerminalPool renders it in an overlay layer, a SIBLING of the
 *     grid). A drop on the live terminal body hits THIS. Its value is the
 *     terminal id. (Tile.tsx's own data-tile-id drag-resolution only works
 *     because index.css makes this wrapper pointer-inert during a tile drag —
 *     `[data-th-dragging]` — which an OS file-drop never sets, so we can't rely
 *     on falling through to data-tile-id here.)
 *   - `data-tile-id` — the grid cell (header chrome, or the body when the tile
 *     shows a non-terminal panel and its terminal is parked offscreen). Also the
 *     terminal id.
 *
 * Pool wrapper wins when both are ancestors (it's the inner one over the body).
 * Returns null when the point isn't over any tile.
 */
function terminalAt(x: number, y: number): TerminalId | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  const hit = el?.closest<HTMLElement>("[data-th-pool-tile], [data-tile-id]");
  if (!hit) return null;
  return (
    hit.getAttribute("data-th-pool-tile") ?? hit.getAttribute("data-tile-id")
  );
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
      // file-drop event to bind; the feature is simply absent there. We leave
      // `dropInstalled` true so we don't re-attempt on every later mount (and so
      // a transient failure can never end up binding the listener twice, which
      // would type each dropped path twice).
    });
}
