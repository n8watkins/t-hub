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

/** Bounds for the configurable titlebar auto-hide timings (used by both the
 *  persistence clamp and the Settings sliders, so they can't drift apart). */
export const TITLEBAR_HIDE_DELAY_MIN = 500;
export const TITLEBAR_HIDE_DELAY_MAX = 6000;
export const TITLEBAR_REVEAL_ANIM_MIN = 40;
export const TITLEBAR_REVEAL_ANIM_MAX = 400;

/** Persisted flag defaults. */
const DEFAULTS = {
  /** Titlebar auto-reveal pushes the body down (vs. overlays it). */
  revealPushesContent: true,
  /** Auto-hide the titlebar when the window is maximized (opt-in). Default OFF
   *  so maximize/restore is always reachable; the user can enable it. */
  autoHideTitlebarMaximized: false,
  /** Delay (ms) before an auto-hidden titlebar hides — both after the initial
   *  maximize reveal and after the pointer leaves the bar. */
  titlebarHideDelayMs: 2000,
  /** Duration (ms) of the titlebar show/hide slide animation. */
  titlebarRevealAnimMs: 140,
  /** Play a short chime on key session events (attention / done / error). */
  soundsEnabled: true,
  /** Show desktop (OS) notifications for key session events. */
  notificationsEnabled: true,
} as const;

interface PersistedSettings {
  revealPushesContent: boolean;
  autoHideTitlebarMaximized: boolean;
  titlebarHideDelayMs: number;
  titlebarRevealAnimMs: number;
  soundsEnabled: boolean;
  notificationsEnabled: boolean;
}

/** Clamp a persisted/incoming number into a range, falling back to a default
 *  when it isn't a finite number. */
function clampNum(v: unknown, min: number, max: number, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v)
    ? Math.max(min, Math.min(max, Math.round(v)))
    : fallback;
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
      titlebarHideDelayMs: clampNum(
        p.titlebarHideDelayMs,
        TITLEBAR_HIDE_DELAY_MIN,
        TITLEBAR_HIDE_DELAY_MAX,
        DEFAULTS.titlebarHideDelayMs,
      ),
      titlebarRevealAnimMs: clampNum(
        p.titlebarRevealAnimMs,
        TITLEBAR_REVEAL_ANIM_MIN,
        TITLEBAR_REVEAL_ANIM_MAX,
        DEFAULTS.titlebarRevealAnimMs,
      ),
      soundsEnabled:
        typeof p.soundsEnabled === "boolean"
          ? p.soundsEnabled
          : DEFAULTS.soundsEnabled,
      notificationsEnabled:
        typeof p.notificationsEnabled === "boolean"
          ? p.notificationsEnabled
          : DEFAULTS.notificationsEnabled,
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

  /** Delay (ms) before an auto-hidden titlebar hides. Clamped on write. */
  titlebarHideDelayMs: number;
  setTitlebarHideDelayMs: (v: number) => void;

  /** Duration (ms) of the titlebar show/hide slide animation. Clamped on write. */
  titlebarRevealAnimMs: number;
  setTitlebarRevealAnimMs: (v: number) => void;

  /** Play a short chime on key session events. Respected by lib/notify.ts. */
  soundsEnabled: boolean;
  setSoundsEnabled: (v: boolean) => void;

  /** Show desktop notifications for key session events. Respected by notify.ts. */
  notificationsEnabled: boolean;
  setNotificationsEnabled: (v: boolean) => void;
}

const initial = loadPersisted();

export const useSettings = create<SettingsState>((set, get) => {
  /** Snapshot every persisted field off the current state. Each setter writes
   *  its new value into the store first, then persists this whole snapshot, so
   *  fields stay in sync no matter which setter fired. */
  const persistAll = () => {
    const s = get();
    savePersisted({
      revealPushesContent: s.revealPushesContent,
      autoHideTitlebarMaximized: s.autoHideTitlebarMaximized,
      titlebarHideDelayMs: s.titlebarHideDelayMs,
      titlebarRevealAnimMs: s.titlebarRevealAnimMs,
      soundsEnabled: s.soundsEnabled,
      notificationsEnabled: s.notificationsEnabled,
    });
  };

  return {
    settingsOpen: false,
    openSettings: () => set({ settingsOpen: true }),
    closeSettings: () => set({ settingsOpen: false }),
    toggleSettings: () => set({ settingsOpen: !get().settingsOpen }),

    revealPushesContent: initial.revealPushesContent,
    setRevealPushesContent: (v) => {
      set({ revealPushesContent: v });
      persistAll();
    },

    autoHideTitlebarMaximized: initial.autoHideTitlebarMaximized,
    setAutoHideTitlebarMaximized: (v) => {
      set({ autoHideTitlebarMaximized: v });
      persistAll();
    },

    titlebarHideDelayMs: initial.titlebarHideDelayMs,
    setTitlebarHideDelayMs: (v) => {
      set({
        titlebarHideDelayMs: clampNum(
          v,
          TITLEBAR_HIDE_DELAY_MIN,
          TITLEBAR_HIDE_DELAY_MAX,
          DEFAULTS.titlebarHideDelayMs,
        ),
      });
      persistAll();
    },

    titlebarRevealAnimMs: initial.titlebarRevealAnimMs,
    setTitlebarRevealAnimMs: (v) => {
      set({
        titlebarRevealAnimMs: clampNum(
          v,
          TITLEBAR_REVEAL_ANIM_MIN,
          TITLEBAR_REVEAL_ANIM_MAX,
          DEFAULTS.titlebarRevealAnimMs,
        ),
      });
      persistAll();
    },

    soundsEnabled: initial.soundsEnabled,
    setSoundsEnabled: (v) => {
      set({ soundsEnabled: v });
      persistAll();
    },

    notificationsEnabled: initial.notificationsEnabled,
    setNotificationsEnabled: (v) => {
      set({ notificationsEnabled: v });
      persistAll();
    },
  };
});
