// The settings store holds T-Hub's *general app settings* — the non-theme
// knobs of the shell (PRD §5.5 settings surface) — kept separate from the theme
// store: the theme store owns the look (CSS vars), this owns behavior flags plus
// the transient open/closed state of the settings panel itself.
//
// Persistence is localStorage for now (SQLite lands later): we persist ONLY the
// behavior flags, best-effort, under a versioned key. Transient UI state (panel
// open) is never persisted — a reopen of the app starts with the panel closed.
import { create } from "zustand";
import {
  loadPersisted as loadFromStorage,
  savePersisted as saveToStorage,
} from "../lib/persist";

const PERSIST_KEY = "t-hub.settings.v1";

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
  /** Play a short chime on key session events (attention / done / error).
   *  Default OFF — notifications/sounds are opt-in (the user enables them in
   *  Settings) so a fresh install is quiet. */
  soundsEnabled: false,
  /** Show desktop (OS) notifications for key session events. Default OFF (opt-in,
   *  paired with soundsEnabled). */
  notificationsEnabled: false,
  /** Show the context-window meter in each tile's HEADER. Default OFF — the
   *  header is tight on space, so the ctx% indicator is opt-in there. The bottom
   *  Claude-config bar / sidebar captain rows show it regardless (untouched by
   *  this flag). Read by components/Tile.tsx. */
  showHeaderContextMeter: false,
  /** The titlebar × hides the window to the system tray (true) vs. quits the app
   *  (false). Default ON — close-to-tray, matching the tray's keep-running model. */
  closeToTray: true,
  /** Periodically check GitHub Releases for a newer signed build (feat/auto-updater). */
  autoUpdateCheckEnabled: true,
  /** Silently download + install a found update on launch, then relaunch. Only
   *  acted on when autoUpdateCheckEnabled is also on. */
  autoInstallUpdates: true,
  /** When resuming a session from the Recent list, start Claude Code in it
   *  (`claude --resume <id>`). When false, Resume just opens a terminal in the
   *  session's directory (no Claude). Default on — Recent is a Claude library. */
  resumeStartsClaude: true,
  /** Fixed root directory for the Files panel. Empty = use the project's own cwd
   *  (per-tile). Set to an absolute path (e.g. /home/natkins) to ALWAYS start the
   *  file tree there. The header shows paths relative to this / the home dir. */
  filesRootDir: "",
  /** File-tree icon theme: "lucide" (minimal, default), "vscode" (colorful), or
   *  "seti" (muted). Read + validated by lib/fileIcons.tsx. */
  fileIconTheme: "lucide",
  /** Hide dotfiles (entries whose name starts with ".", e.g. .git, .cargo,
   *  .claude) in the Files panel/tree. Default ON — the file tree is a lot
   *  quieter without them; a header toggle reveals them. Read by FileTree.tsx
   *  and FilePanel.tsx, which filter entries at render at every level. */
  hideDotfiles: true,
  /** The command auto-continue injects when an opted-in terminal's Claude session
   *  hits its usage limit and the window resets (see lib/autoContinueMount). The
   *  text is typed + Enter; default "continue". */
  autoContinueText: "continue",
} as const;

interface PersistedSettings {
  revealPushesContent: boolean;
  autoHideTitlebarMaximized: boolean;
  titlebarHideDelayMs: number;
  titlebarRevealAnimMs: number;
  soundsEnabled: boolean;
  notificationsEnabled: boolean;
  showHeaderContextMeter: boolean;
  closeToTray: boolean;
  autoUpdateCheckEnabled: boolean;
  autoInstallUpdates: boolean;
  resumeStartsClaude: boolean;
  fileIconTheme: string;
  hideDotfiles: boolean;
  autoContinueText: string;
}

/** Clamp a persisted/incoming number into a range, falling back to a default
 *  when it isn't a finite number. */
function clampNum(v: unknown, min: number, max: number, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v)
    ? Math.max(min, Math.min(max, Math.round(v)))
    : fallback;
}

/** Validate one persisted blob field-by-field, falling back per-field to
 *  DEFAULTS (and clamping the numeric timings). Owns this store's coerce logic;
 *  the SSR guard + corrupt-fallback plumbing lives in lib/persist. */
function coerceSettings(raw: unknown): PersistedSettings {
  const p = (raw ?? {}) as Partial<PersistedSettings>;
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
    showHeaderContextMeter:
      typeof p.showHeaderContextMeter === "boolean"
        ? p.showHeaderContextMeter
        : DEFAULTS.showHeaderContextMeter,
    closeToTray:
      typeof p.closeToTray === "boolean"
        ? p.closeToTray
        : DEFAULTS.closeToTray,
    autoUpdateCheckEnabled:
      typeof p.autoUpdateCheckEnabled === "boolean"
        ? p.autoUpdateCheckEnabled
        : DEFAULTS.autoUpdateCheckEnabled,
    autoInstallUpdates:
      typeof p.autoInstallUpdates === "boolean"
        ? p.autoInstallUpdates
        : DEFAULTS.autoInstallUpdates,
    resumeStartsClaude:
      typeof p.resumeStartsClaude === "boolean"
        ? p.resumeStartsClaude
        : DEFAULTS.resumeStartsClaude,
    fileIconTheme:
      typeof p.fileIconTheme === "string"
        ? p.fileIconTheme
        : DEFAULTS.fileIconTheme,
    hideDotfiles:
      typeof p.hideDotfiles === "boolean"
        ? p.hideDotfiles
        : DEFAULTS.hideDotfiles,
    autoContinueText:
      typeof p.autoContinueText === "string"
        ? p.autoContinueText
        : DEFAULTS.autoContinueText,
  };
}

function loadPersisted(): PersistedSettings {
  return loadFromStorage(PERSIST_KEY, { ...DEFAULTS }, coerceSettings);
}

function savePersisted(s: PersistedSettings): void {
  saveToStorage(PERSIST_KEY, s);
}

interface SettingsState {
  /** Whether the settings panel is open. Transient — NOT persisted. */
  settingsOpen: boolean;
  openSettings: () => void;
  closeSettings: () => void;
  toggleSettings: () => void;
  /** Deep-link target: the settings nav section to show when the panel next
   *  opens (e.g. open straight to "hooks" from the sidebar usage hint). Read by
   *  ThemeEditorPanel on mount, then cleared on close. Transient. */
  settingsSection: string | null;
  /** Open the settings panel directly to a given nav section. */
  openSettingsTo: (section: string) => void;

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

  /** Show the context-window meter in each tile's header (default OFF). Read by
   *  components/Tile.tsx; the bottom/sidebar readouts ignore it. */
  showHeaderContextMeter: boolean;
  setShowHeaderContextMeter: (v: boolean) => void;

  /** The titlebar × hides to tray (true) vs. quits (false). Read by Titlebar's
   *  close handler. */
  closeToTray: boolean;
  setCloseToTray: (v: boolean) => void;

  /** Periodically check for app updates. Respected by lib/updateMount.ts and the
   *  Updates settings section. */
  autoUpdateCheckEnabled: boolean;
  setAutoUpdateCheckEnabled: (v: boolean) => void;

  /** Silently install a found update on launch (only when autoUpdateCheckEnabled
   *  is on). Respected by lib/updateMount.ts. */
  autoInstallUpdates: boolean;
  setAutoInstallUpdates: (v: boolean) => void;

  /** Resume from Recent starts Claude Code (vs. just opening a terminal in the
   *  session's dir). Read by workspace.ts `recall`. */
  resumeStartsClaude: boolean;
  setResumeStartsClaude: (v: boolean) => void;

  /** File-tree icon theme ("lucide" | "vscode" | "seti"). Read by lib/fileIcons. */
  fileIconTheme: string;
  setFileIconTheme: (v: string) => void;

  /** Hide dotfiles (".*") in the Files panel/tree (default true). Read by
   *  FileTree.tsx and FilePanel.tsx; flipped by the Files header toggle. */
  hideDotfiles: boolean;
  setHideDotfiles: (v: boolean) => void;

  /** Command auto-continue types when an opted-in session resets (default
   *  "continue"). See store/autoContinue + lib/autoContinueMount. */
  autoContinueText: string;
  setAutoContinueText: (v: string) => void;
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
      showHeaderContextMeter: s.showHeaderContextMeter,
      closeToTray: s.closeToTray,
      autoUpdateCheckEnabled: s.autoUpdateCheckEnabled,
      autoInstallUpdates: s.autoInstallUpdates,
      resumeStartsClaude: s.resumeStartsClaude,
      fileIconTheme: s.fileIconTheme,
      hideDotfiles: s.hideDotfiles,
      autoContinueText: s.autoContinueText,
    });
  };

  return {
    settingsOpen: false,
    settingsSection: null,
    openSettings: () => set({ settingsOpen: true }),
    closeSettings: () => set({ settingsOpen: false, settingsSection: null }),
    toggleSettings: () => set({ settingsOpen: !get().settingsOpen }),
    openSettingsTo: (section) => set({ settingsOpen: true, settingsSection: section }),

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

    showHeaderContextMeter: initial.showHeaderContextMeter,
    setShowHeaderContextMeter: (v) => {
      set({ showHeaderContextMeter: v });
      persistAll();
    },

    closeToTray: initial.closeToTray,
    setCloseToTray: (v) => {
      set({ closeToTray: v });
      persistAll();
    },

    autoUpdateCheckEnabled: initial.autoUpdateCheckEnabled,
    setAutoUpdateCheckEnabled: (v) => {
      set({ autoUpdateCheckEnabled: v });
      persistAll();
    },

    autoInstallUpdates: initial.autoInstallUpdates,
    setAutoInstallUpdates: (v) => {
      set({ autoInstallUpdates: v });
      persistAll();
    },

    resumeStartsClaude: initial.resumeStartsClaude,
    setResumeStartsClaude: (v) => {
      set({ resumeStartsClaude: v });
      persistAll();
    },

    fileIconTheme: initial.fileIconTheme,
    setFileIconTheme: (v) => {
      set({ fileIconTheme: v });
      persistAll();
    },

    hideDotfiles: initial.hideDotfiles,
    setHideDotfiles: (v) => {
      set({ hideDotfiles: v });
      persistAll();
    },

    autoContinueText: initial.autoContinueText,
    setAutoContinueText: (v) => {
      set({ autoContinueText: v });
      persistAll();
    },
  };
});
