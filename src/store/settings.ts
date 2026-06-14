// The settings store holds TermHub's *general app settings* — the non-theme
// knobs of the shell (PRD §5.5 settings surface). It is deliberately separate
// from the theme store: the theme store owns the look (colors/sizes written as
// CSS vars), whereas this store owns behavior flags + the transient open/closed
// state of the settings panel itself.
//
// Like the theme + workspace stores, persistence is localStorage for now
// (SQLite lands later): we persist ONLY the behavior flags, best-effort, under a
// versioned key. Transient UI state (whether the panel is open) is never
// persisted — a reopen of the app should start with the panel closed.
import { create } from "zustand";

// ---------------------------------------------------------------------------
// Persistence (localStorage) — behavior flags only
// ---------------------------------------------------------------------------

const PERSIST_KEY = "termhub.settings.v1";

/** Default for `revealPushesContent` (titlebar auto-reveal pushes body down). */
const DEFAULT_REVEAL_PUSHES_CONTENT = true;

/** The subset of settings we persist across UI reopens (flags only). */
interface PersistedSettings {
  revealPushesContent: boolean;
}

function loadPersisted(): PersistedSettings {
  if (typeof localStorage === "undefined") {
    return { revealPushesContent: DEFAULT_REVEAL_PUSHES_CONTENT };
  }
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (!raw) return { revealPushesContent: DEFAULT_REVEAL_PUSHES_CONTENT };
    const parsed = JSON.parse(raw) as Partial<PersistedSettings>;
    return {
      revealPushesContent:
        typeof parsed.revealPushesContent === "boolean"
          ? parsed.revealPushesContent
          : DEFAULT_REVEAL_PUSHES_CONTENT,
    };
  } catch {
    return { revealPushesContent: DEFAULT_REVEAL_PUSHES_CONTENT };
  }
}

function savePersisted(settings: PersistedSettings): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(PERSIST_KEY, JSON.stringify(settings));
  } catch {
    // localStorage full / unavailable — non-fatal; setting stays in memory.
  }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

interface SettingsState {
  /**
   * Whether the settings panel is open. Transient UI state — NOT persisted, so
   * the app always boots with the panel closed.
   */
  settingsOpen: boolean;
  /** Open the settings panel. */
  openSettings: () => void;
  /** Close the settings panel. */
  closeSettings: () => void;
  /** Toggle the settings panel open/closed (the Ctrl/Cmd+, handler). */
  toggleSettings: () => void;

  /**
   * When the titlebar auto-reveals in maximized mode, whether it pushes body
   * content down via a layout shift (true) vs. overlaying it (false). Persisted;
   * the flag + setter live here, the titlebar reveal behavior consumes it.
   */
  revealPushesContent: boolean;
  /** Set whether the titlebar reveal pushes content down (persisted). */
  setRevealPushesContent: (v: boolean) => void;
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
    savePersisted({ revealPushesContent: v });
  },
}));
