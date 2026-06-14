// Per-project panel state — the per-tile "workbench".
//
// Each project tile shows ONE of several views — Terminal / Files / Preview /
// Dev — plus a fullscreen toggle. This store holds that purely-presentational,
// per-terminal UI state. It is deliberately kept OUT of the workspace store so
// the parallel panel + dev-runner work doesn't contend on workspace.ts.
// In-memory only for v1 (not persisted): a fresh launch starts every tile on its
// Terminal view, which is the safe default.
//
// CROSS-FEATURE CONTRACT (scaffolded for the parallel build — multiple
// components import this; don't reshape it lightly):
//   - The per-tile panel (Tile/TilePanel) reads/sets `tab` + `fullscreenId`.
//   - The Dev runner (DevTab) publishes the detected dev-server URL via
//     `setDevUrl`.
//   - The Preview tab reads `devUrl[id]` (falling back to the user-typed
//     `previewUrl[id]`), so a freshly-detected dev server loads automatically.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";

/** The selectable views inside a project tile. */
export type PanelTab = "terminal" | "files" | "preview" | "dev";

/** The view a tile shows until the user switches it. */
export const DEFAULT_PANEL_TAB: PanelTab = "terminal";

interface PanelState {
  /** Active view per terminal id. Missing => DEFAULT_PANEL_TAB ("terminal"). */
  tab: Record<TerminalId, PanelTab>;
  /** The one tile expanded to fill the window, or null for the normal grid. */
  fullscreenId: TerminalId | null;
  /** Dev-server URL detected for a terminal (null/absent => none yet). Written
   *  by the Dev runner; read by the Preview tab. */
  devUrl: Record<TerminalId, string | null>;
  /** Last URL the user committed in a terminal's Preview tab, so it survives a
   *  tab switch. The Preview tab prefers a live `devUrl` over this. */
  previewUrl: Record<TerminalId, string | null>;

  /** Active view for a terminal, defaulted. */
  getTab: (id: TerminalId) => PanelTab;
  /** Switch a terminal's active view. */
  setTab: (id: TerminalId, tab: PanelTab) => void;
  /** Toggle fullscreen for a terminal (clears it if it's already fullscreen). */
  toggleFullscreen: (id: TerminalId) => void;
  /** Set (or clear, with null) the fullscreen tile explicitly. */
  setFullscreen: (id: TerminalId | null) => void;
  /** Record the dev-server URL detected for a terminal (null clears it). */
  setDevUrl: (id: TerminalId, url: string | null) => void;
  /** Remember the user-typed Preview URL for a terminal. */
  setPreviewUrl: (id: TerminalId, url: string | null) => void;
  /** Drop all panel state for a terminal (call when its tile is deleted). */
  forget: (id: TerminalId) => void;
}

export const usePanels = create<PanelState>((set, get) => ({
  tab: {},
  fullscreenId: null,
  devUrl: {},
  previewUrl: {},

  getTab: (id) => get().tab[id] ?? DEFAULT_PANEL_TAB,
  setTab: (id, tab) => set((s) => ({ tab: { ...s.tab, [id]: tab } })),
  toggleFullscreen: (id) =>
    set((s) => ({ fullscreenId: s.fullscreenId === id ? null : id })),
  setFullscreen: (id) => set({ fullscreenId: id }),
  setDevUrl: (id, url) => set((s) => ({ devUrl: { ...s.devUrl, [id]: url } })),
  setPreviewUrl: (id, url) =>
    set((s) => ({ previewUrl: { ...s.previewUrl, [id]: url } })),
  forget: (id) =>
    set((s) => {
      const tab = { ...s.tab };
      const devUrl = { ...s.devUrl };
      const previewUrl = { ...s.previewUrl };
      delete tab[id];
      delete devUrl[id];
      delete previewUrl[id];
      return {
        tab,
        devUrl,
        previewUrl,
        fullscreenId: s.fullscreenId === id ? null : s.fullscreenId,
      };
    }),
}));
