// The settings store holds TermHub's *general app settings* — the non-theme
// knobs of the shell (PRD §5.5 settings surface) — kept separate from the theme
// store: the theme store owns the look (CSS vars), this owns behavior flags plus
// the transient open/closed state of the settings panel itself.
//
// Persistence is localStorage for now (SQLite lands later): we persist ONLY the
// behavior flags, best-effort, under a versioned key. Transient UI state (panel
// open) is never persisted — a reopen of the app starts with the panel closed.
import { create } from "zustand";

const PERSIST_KEY = "termhub.settings.v1";

/** Persisted flag defaults. */
const DEFAULTS = {
  /** Titlebar auto-reveal pushes the body down (vs. overlays it). */
  revealPushesContent: true,
  /** Auto-hide the titlebar when the window is maximized (opt-in). Default OFF
   *  so maximize/restore is always reachable; the user can enable it. */
  autoHideTitlebarMaximized: false,
} as const;

interface PersistedSettings {
  revealPushesContent: boolean;
  autoHideTitlebarMaximized: boolean;
}

function loadPersisted(): PersistedSettings {
  if (typeof localStorage === "undefined") return { ...DEFAULTS };
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (!raw) return { ...DEFAULTS };
    const p = JSON.parse(raw) as Partial<PersistedSettings>;
    return {
      revealPushesContent:
        typeof p.revealPushesContent === "boolean"
          ? p.revealPushesContent
          : DEFAULTS.revealPushesContent,
      autoHideTitlebarMaximized:
        typeof p.autoHideTitlebarMaximized === "boolean"
          ? p.autoHideTitlebarMaximized
          : DEFAULTS.autoHideTitlebarMaximized,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

function savePersisted(s: PersistedSettings): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(PERSIST_KEY, JSON.stringify(s));
  } catch {
    // localStorage full / unavailable — non-fatal; settings stay in memory.
  }
}

interface SettingsState {
  /** Whether the settings panel is open. Transient — NOT persisted. */
  settingsOpen: boolean;
  openSettings: () => void;
  closeSettings: () => void;
  toggleSettings: () => void;

  /** Titlebar auto-reveal pushes content down (true) vs. overlays it (false). */
  revealPushesContent: boolean;
  setRevealPushesContent: (v: boolean) => void;

  /** Auto-hide the titlebar when maximized (opt-in; default false). */
  autoHideTitlebarMaximized: boolean;
  setAutoHideTitlebarMaximized: (v: boolean) => void;
}

const initial = loadPersisted();

export const useSettings = create<SettingsState>((set, get) => ({
  settingsOpen: false,
  openSettings: () => set({ settingsOpen: true }),
  closeSettings: () => set({ settingsOpen: false }),
  toggleSettings: () => set({ settingsOpen: !get().settingsOpen }),

  revealPushesContent: initial.revealPushesContent,
  setRevealPushesContent: (v) => {
    set({ revealPushesContent: v });
    savePersisted({
      revealPushesContent: v,
      autoHideTitlebarMaximized: get().autoHideTitlebarMaximized,
    });
  },

  autoHideTitlebarMaximized: initial.autoHideTitlebarMaximized,
  setAutoHideTitlebarMaximized: (v) => {
    set({ autoHideTitlebarMaximized: v });
    savePersisted({
      revealPushesContent: get().revealPushesContent,
      autoHideTitlebarMaximized: v,
    });
  },
}));
