// Detached / popped-out preview windows (feat/preview, TASK 2 + 3).
//
// The in-tile Preview tab (WebPreview inside TilePanel) shows a dev server in an
// iframe. This module lets the user "pop out" a preview into its OWN OS window —
// a real, decorated, resizable Tauri WebviewWindow that loads the dev URL as a
// TOP-LEVEL navigation (no iframe → no X-Frame-Options/CSP framing limits) so
// they can watch what they're building independently of the tile.
//
// MULTIPLE previews (TASK 2): each pop-out is a distinct OS window with its own
// label, so any number can be open at once. We also keep a tiny registry of the
// open windows here (label → url) so the app can reason about / focus / close
// them. The in-tile previews were already independent per tile; this removes the
// "only one preview" ceiling by making detached previews first-class and
// unbounded.
//
// WINDOW LABELS: we use the `th-pop-preview-<n>` prefix, which matches the
// existing `th-pop-*` capability glob in src-tauri/capabilities/default.json —
// so NO new capability/permission is needed (the tab tear-off feature, #21,
// already provisioned `th-pop-*` with `core:webview:allow-create-webview-window`
// + `core:window:*`). If that glob ever narrows, these windows would lose their
// permissions; see the final hand-off note.
//
// We point the WebviewWindow's `url` straight at the (already host-rewritten)
// dev URL rather than reloading the T-Hub bundle: a preview wants the user's
// app, not another T-Hub shell, and a top-level load sidesteps the framing
// refusals an iframe hits. The URL must already be reachable from the WINDOWS
// host (see ipc/devserver.ts `reachablePreviewUrl`) since the WebView is a
// Windows process — callers pass the resolved URL.
import { create } from "zustand";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

/** Window-label prefix for popped-out previews. Matches the `th-pop-*` glob in
 *  capabilities/default.json so these windows inherit the needed permissions. */
const PREVIEW_PREFIX = "th-pop-preview-";

/** A single open detached-preview window. */
interface OpenPreview {
  /** The Tauri window label (e.g. `th-pop-preview-3`). */
  label: string;
  /** The URL it was opened with (already host-rewritten to be reachable). */
  url: string;
}

interface PreviewWindowsState {
  /** Open detached previews, keyed by window label. */
  open: Record<string, OpenPreview>;
  /** Monotonic counter so each pop-out gets a unique label (even after closes). */
  seq: number;
  /** Record a freshly opened preview window. */
  track: (label: string, url: string) => void;
  /** Drop a preview window from the registry (it was closed/destroyed). */
  untrack: (label: string) => void;
  /** How many detached previews are currently open. */
  count: () => number;
}

export const usePreviewWindows = create<PreviewWindowsState>((set, get) => ({
  open: {},
  seq: 0,
  track: (label, url) =>
    set((s) => ({ open: { ...s.open, [label]: { label, url } } })),
  untrack: (label) =>
    set((s) => {
      if (!(label in s.open)) return s;
      const open = { ...s.open };
      delete open[label];
      return { open };
    }),
  count: () => Object.keys(get().open).length,
}));

/**
 * Pop a preview out into its own OS window, loading `url` as a top-level page.
 *
 * `url` should already be reachable from the Windows host — callers resolve it
 * via `reachablePreviewUrl` (ipc/devserver.ts) first so a WSL `localhost` server
 * is hit on an address the WebView can actually reach. Each call opens a NEW
 * window (multiple previews allowed); the window is tracked in this store and
 * auto-untracked when it is destroyed.
 *
 * Returns the new window's label, or null if creation failed / there was no URL.
 */
export async function popOutPreview(url: string, title?: string): Promise<string | null> {
  const target = (url ?? "").trim();
  if (!target) return null;

  // Unique label per pop-out so any number can coexist. Bump the counter in the
  // store so closes don't recycle a label a still-open window might hold.
  const seq = usePreviewWindows.getState().seq + 1;
  usePreviewWindows.setState({ seq });
  const label = `${PREVIEW_PREFIX}${seq}`;

  // A decorated (OS-chrome) window so it reads as a normal browser-like preview
  // the user can move/resize/close on its own — unlike the frameless main/tab
  // windows. Loads the dev URL directly (top-level nav → no framing refusal).
  const win = new WebviewWindow(label, {
    url: target,
    title: title ? `Preview — ${title}` : "Preview",
    width: 1100,
    height: 800,
    minWidth: 360,
    minHeight: 240,
    // Decorated on purpose (the preview isn't a T-Hub surface; give it native
    // window controls rather than relying on T-Hub's <Titlebar/>).
    decorations: true,
  });

  // Track on success; clean the registry up if creation fails or the window is
  // later closed by the user (so `count()` stays honest).
  return await new Promise<string | null>((resolve) => {
    win.once("tauri://created", () => {
      usePreviewWindows.getState().track(label, target);
      // When the user closes the OS window, drop it from the registry.
      void win
        .onCloseRequested(() => {
          usePreviewWindows.getState().untrack(label);
        })
        .catch(() => {});
      resolve(label);
    });
    win.once("tauri://error", (e) => {
      // eslint-disable-next-line no-console
      console.error("popOutPreview: failed to create preview window", e);
      usePreviewWindows.getState().untrack(label);
      resolve(null);
    });
  });
}
