// Per-project panel state — the per-tile "workbench".
//
// Each project tile shows one of Terminal / Files / Run + Preview / Board, plus
// a fullscreen toggle. This store holds that purely-presentational,
// per-terminal UI state. It is deliberately kept OUT of the workspace store so
// the parallel panel work doesn't contend on workspace.ts.
// In-memory only for v1 (not persisted): a fresh launch starts every tile on its
// Terminal view, which is the safe default.
//
// CROSS-FEATURE CONTRACT (scaffolded for the parallel build — multiple
// components import this; don't reshape it lightly):
//   - The per-tile panel (Tile/TilePanel) reads/sets `tab` + `fullscreenId`.
//   - RunPreviewPanel runs the project's managed dev server and previews only
//     the URL reported by that owned process or explicitly entered by the user.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";

/** The selectable views inside a project tile. */
export type PanelTab = "terminal" | "files" | "preview" | "board";

/** The view a tile shows until the user switches it. */
export const DEFAULT_PANEL_TAB: PanelTab = "terminal";

/**
 * Split-ratio bounds for the draggable terminal|panel divider (SPLIT mode). The
 * ratio is the TERMINAL half's fraction of the tile width (0..1); the panel gets
 * the rest. Clamped so neither side can be dragged to uselessness — each keeps a
 * readable minimum (a tile cell is already narrow). Default 0.5 = even split,
 * matching the previous fixed `flex-1` terminal + `w-1/2` panel layout.
 */
export const SPLIT_RATIO_MIN = 0.25;
export const SPLIT_RATIO_MAX = 0.75;
export const DEFAULT_SPLIT_RATIO = 0.5;

/** Clamp a raw split ratio into [MIN, MAX]; non-finite falls back to the default. */
export function clampSplitRatio(r: number): number {
  if (!Number.isFinite(r)) return DEFAULT_SPLIT_RATIO;
  return Math.max(SPLIT_RATIO_MIN, Math.min(SPLIT_RATIO_MAX, r));
}

/** localStorage key for the per-tile split ratios. Only the split ratio is
 *  persisted from this otherwise in-memory store — it's a deliberate, sticky
 *  layout preference (a user who widened a tile's Files panel wants it to stay),
 *  unlike the transient active-tab/fullscreen/url state. */
const SPLIT_PERSIST_KEY = "t-hub.panels.splitRatio.v1";

/** Read the persisted split-ratio map (best-effort; {} on any error/absence). */
function loadSplitRatios(): Record<TerminalId, number> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(SPLIT_PERSIST_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<TerminalId, number> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "number" && Number.isFinite(v)) out[k] = clampSplitRatio(v);
    }
    return out;
  } catch {
    return {};
  }
}

/** Persist the split-ratio map (best-effort; ignore quota/serialization errors). */
function saveSplitRatios(map: Record<TerminalId, number>): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(SPLIT_PERSIST_KEY, JSON.stringify(map));
  } catch {
    /* ignore */
  }
}

interface PanelState {
  /** Active view per terminal id. Missing => DEFAULT_PANEL_TAB ("terminal"). */
  tab: Record<TerminalId, PanelTab>;
  /** The one tile expanded to fill the window, or null for the normal grid. */
  fullscreenId: TerminalId | null;
  /** Dev-server URL reported by the managed runner for this terminal. */
  devUrl: Record<TerminalId, string | null>;
  /** Last URL the user committed in Run and Preview, so it survives a tab switch.
   *  The managed runner's live `devUrl` takes precedence. */
  previewUrl: Record<TerminalId, string | null>;
  /** Per-tile: when a non-terminal tab is active the tile SPLITS (terminal +
   *  panel). `panelExpanded[id]` true means the panel is EXPANDED to fill the
   *  whole tile (terminal hidden); false/absent => the split. */
  panelExpanded: Record<TerminalId, boolean>;
  /** Per-tile SPLIT divider position: the TERMINAL half's width fraction (0..1)
   *  of the tile in SPLIT mode; the panel gets the remainder. Absent =>
   *  DEFAULT_SPLIT_RATIO (even). Persisted (see saveSplitRatios). */
  splitRatio: Record<TerminalId, number>;

  /** Active view for a terminal, defaulted. */
  getTab: (id: TerminalId) => PanelTab;
  /** The SPLIT terminal-half width fraction for a tile, defaulted + clamped. */
  getSplitRatio: (id: TerminalId) => number;
  /** Set a tile's SPLIT divider position (terminal-half fraction), clamped to
   *  [SPLIT_RATIO_MIN, SPLIT_RATIO_MAX] and persisted. */
  setSplitRatio: (id: TerminalId, ratio: number) => void;
  /** Switch a terminal's active view. Switching to a non-terminal tab leaves the
   *  expand state as-is; switching to "terminal" clears expand (back to a clean
   *  terminal). */
  setTab: (id: TerminalId, tab: PanelTab) => void;
  /** Toggle whether this tile's panel is expanded (fills the tile) vs split. */
  togglePanelExpanded: (id: TerminalId) => void;
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
  panelExpanded: {},
  splitRatio: loadSplitRatios(),

  getTab: (id) => get().tab[id] ?? DEFAULT_PANEL_TAB,
  getSplitRatio: (id) =>
    clampSplitRatio(get().splitRatio[id] ?? DEFAULT_SPLIT_RATIO),
  setSplitRatio: (id, ratio) =>
    set((s) => {
      const next = { ...s.splitRatio, [id]: clampSplitRatio(ratio) };
      saveSplitRatios(next);
      return { splitRatio: next };
    }),
  setTab: (id, tab) =>
    set((s) => ({
      tab: { ...s.tab, [id]: tab },
      // Returning to the terminal clears any expanded panel so the next time you
      // open a panel you get the split, not a surprise full-takeover.
      panelExpanded:
        tab === "terminal"
          ? { ...s.panelExpanded, [id]: false }
          : s.panelExpanded,
    })),
  togglePanelExpanded: (id) =>
    set((s) => ({
      // Default is EXPANDED (true) — see Tile/TerminalPool — so the first toggle
      // flips to the split.
      panelExpanded: { ...s.panelExpanded, [id]: !(s.panelExpanded[id] ?? true) },
    })),
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
      const panelExpanded = { ...s.panelExpanded };
      const splitRatio = { ...s.splitRatio };
      delete tab[id];
      delete devUrl[id];
      delete previewUrl[id];
      delete panelExpanded[id];
      const hadRatio = id in splitRatio;
      delete splitRatio[id];
      // Drop the persisted ratio too so a recycled id doesn't inherit it.
      if (hadRatio) saveSplitRatios(splitRatio);
      return {
        tab,
        devUrl,
        previewUrl,
        panelExpanded,
        splitRatio,
        fullscreenId: s.fullscreenId === id ? null : s.fullscreenId,
      };
    }),
}));
