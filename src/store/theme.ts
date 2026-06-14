// The theme store is TermHub's live-customization spine (PRD §5.5 settings/
// themes). The whole point: people retheme the UI *without touching config
// files* — and, in parallel, Claude can drive it over MCP. So a theme is just a
// flat bag of "chrome tokens" (colors/sizes/fonts for the app shell) plus an
// optional terminal palette, and the single source of truth lives here in a
// Zustand store.
//
// How it goes live with zero reload:
//   theme tokens  ->  applyTheme() writes each one as a CSS custom property on
//   :root (`--th-accent`, `--th-tile-header-h`, ...)  ->  components read those
//   vars via Tailwind arbitrary values (`bg-[var(--th-app-bg)]`,
//   `ring-[color:var(--th-focus-ring)]`, ...). Because every consumer reads the
//   *variable*, mutating one token re-renders the affected pixels instantly with
//   no React remount and no app restart.
//
// Persistence is localStorage (mirrors the workspace store; SQLite lands later):
// we persist the active theme + any user-saved presets. On boot we also ask the
// backend for its persisted theme (the MCP-writable copy) and subscribe to
// `theme://changed`, so a theme set by Claude/MCP applies here too.
import { create } from "zustand";
import { ThemeEvents } from "../ipc/theme";

// ---------------------------------------------------------------------------
// Theme model
// ---------------------------------------------------------------------------

/**
 * The 16-color ANSI palette (xterm `ITheme` colors), plus the eight bright
 * variants. Optional on a Theme — when present it themes the terminals too; when
 * absent, terminals keep their own default palette.
 */
export interface AnsiPalette {
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

/** The optional terminal palette: xterm background/foreground/cursor + ANSI. */
export interface TerminalPalette {
  background: string;
  foreground: string;
  cursor: string;
  /** Selection highlight (xterm `selectionBackground`). */
  selection: string;
  ansi: AnsiPalette;
}

/**
 * The chrome tokens — every customizable knob of the app shell. Colors are CSS
 * color strings; sizes are numbers in px (except `gridGap`, also px). These map
 * 1:1 onto CSS custom properties via {@link CHROME_VAR}.
 */
export interface ChromeTokens {
  /** Brand/accent color (active tab dot, hover affordances, primary buttons). */
  accent: string;
  /** Focus ring color for the focused tile. */
  focusRing: string;
  /** Focus ring width in px. */
  focusRingWidth: number;

  /** App background (the canvas backdrop behind tiles). */
  appBg: string;
  /** A tile's body/background (behind the terminal). */
  tileBg: string;
  /** A tile header's background. */
  headerBg: string;
  /** The titlebar + sidebar surface background. */
  sidebarBg: string;
  /** The titlebar (top bar) background; defaults near sidebar but separable. */
  titlebarBg: string;

  /** Primary text color. */
  fgPrimary: string;
  /** Muted/secondary text color (cwd, captions). */
  fgMuted: string;

  /** Hairline border color used across chrome (tile/header/sidebar borders). */
  border: string;

  /** Tile header height in px. */
  tileHeaderHeight: number;
  /** Whether tile headers are shown at all (false => headerOnHover takes over). */
  showTileHeader: boolean;
  /** Reveal the header only on tile hover (compact mode). */
  headerOnHover: boolean;
  /** Show the cwd in the tile header. */
  showCwd: boolean;

  /** Grid gap between tiles in px. */
  gridGap: number;
  /** Corner radius (px) applied to tiles + chrome surfaces. */
  cornerRadius: number;

  /** UI font family (CSS font-family list). */
  fontFamily: string;
  /** Base UI font size in px. */
  fontSize: number;

  /** Status-dot colors per terminal lifecycle state. */
  dotStarting: string;
  dotLive: string;
  dotDetached: string;
  dotExited: string;
  dotError: string;
}

/** A complete theme: chrome tokens + an optional terminal palette + a name. */
export interface Theme {
  /** Human-readable name (also the preset key when saved). */
  name: string;
  chrome: ChromeTokens;
  /** Optional terminal palette; omitted => terminals keep their own default. */
  terminal?: TerminalPalette;
}

// ---------------------------------------------------------------------------
// Token -> CSS variable mapping
// ---------------------------------------------------------------------------

/**
 * Maps each {@link ChromeTokens} key to its CSS custom property name. The
 * component refactor reads these exact names (e.g. `--th-focus-ring`), and
 * {@link applyTheme} writes them. Numeric tokens get a `px` suffix on write so
 * they drop straight into `height`/`gap`/`border-width`/etc.
 */
export const CHROME_VAR: Record<keyof ChromeTokens, string> = {
  accent: "--th-accent",
  focusRing: "--th-focus-ring",
  focusRingWidth: "--th-focus-ring-w",
  appBg: "--th-app-bg",
  tileBg: "--th-tile-bg",
  headerBg: "--th-header-bg",
  sidebarBg: "--th-sidebar-bg",
  titlebarBg: "--th-titlebar-bg",
  fgPrimary: "--th-fg",
  fgMuted: "--th-fg-muted",
  border: "--th-border",
  tileHeaderHeight: "--th-tile-header-h",
  showTileHeader: "--th-show-tile-header", // 0/1 flag (Tile consumes via data-tile-header)
  headerOnHover: "--th-header-on-hover", // 0/1 flag (Tile consumes via data-header-hover)
  showCwd: "--th-show-cwd", // 0/1 flag (consumed by Tile via React)
  gridGap: "--th-grid-gap",
  cornerRadius: "--th-radius",
  fontFamily: "--th-font",
  fontSize: "--th-font-size",
  dotStarting: "--th-dot-starting",
  dotLive: "--th-dot-live",
  dotDetached: "--th-dot-detached",
  dotExited: "--th-dot-exited",
  dotError: "--th-dot-error",
};

/** Keys whose value is a px length (numeric token written with a `px` suffix). */
const PX_KEYS: ReadonlySet<keyof ChromeTokens> = new Set([
  "focusRingWidth",
  "tileHeaderHeight",
  "gridGap",
  "cornerRadius",
  "fontSize",
]);

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/** "Midnight" — the default. Matches (or slightly refines) today's all-black look. */
const MIDNIGHT: Theme = {
  name: "Midnight",
  chrome: {
    accent: "#10b981", // emerald-500, the existing accent
    focusRing: "#10b981",
    focusRingWidth: 1,
    appBg: "#0a0a0a", // neutral-950
    tileBg: "#171717", // neutral-900
    headerBg: "#0a0a0a99", // neutral-950/60
    sidebarBg: "#0a0a0a",
    titlebarBg: "#171717", // neutral-900
    fgPrimary: "#e5e5e5", // neutral-200
    fgMuted: "#737373", // neutral-500
    border: "#262626", // neutral-800
    tileHeaderHeight: 22,
    showTileHeader: true,
    headerOnHover: false,
    showCwd: true,
    gridGap: 4,
    cornerRadius: 2,
    fontFamily:
      'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
    fontSize: 12,
    dotStarting: "#f59e0b", // amber-500
    dotLive: "#10b981", // emerald-500
    dotDetached: "#a3a3a3", // neutral-400
    dotExited: "#525252", // neutral-600
    dotError: "#ef4444", // red-500
  },
  terminal: {
    background: "#0a0a0a",
    foreground: "#e5e5e5",
    cursor: "#10b981",
    selection: "#264f78",
    ansi: {
      black: "#171717",
      red: "#ef4444",
      green: "#10b981",
      yellow: "#f59e0b",
      blue: "#3b82f6",
      magenta: "#a855f7",
      cyan: "#06b6d4",
      white: "#d4d4d4",
      brightBlack: "#525252",
      brightRed: "#f87171",
      brightGreen: "#34d399",
      brightYellow: "#fbbf24",
      brightBlue: "#60a5fa",
      brightMagenta: "#c084fc",
      brightCyan: "#22d3ee",
      brightWhite: "#fafafa",
    },
  },
};

/** "Slate" — a cooler, softer blue-grey alternate. */
const SLATE: Theme = {
  name: "Slate",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#38bdf8", // sky-400
    focusRing: "#38bdf8",
    appBg: "#0f172a", // slate-900
    tileBg: "#1e293b", // slate-800
    headerBg: "#0f172acc",
    sidebarBg: "#0f172a",
    titlebarBg: "#1e293b",
    fgPrimary: "#e2e8f0", // slate-200
    fgMuted: "#94a3b8", // slate-400
    border: "#334155", // slate-700
    cornerRadius: 6,
    dotLive: "#38bdf8",
  },
  terminal: {
    background: "#0f172a",
    foreground: "#e2e8f0",
    cursor: "#38bdf8",
    selection: "#334155",
    ansi: { ...MIDNIGHT.terminal!.ansi, green: "#38bdf8", blue: "#60a5fa" },
  },
};

/** "Paper" — a light theme, proving tokens carry the whole look (incl. dark→light). */
const PAPER: Theme = {
  name: "Paper",
  chrome: {
    accent: "#2563eb", // blue-600
    focusRing: "#2563eb",
    focusRingWidth: 2,
    appBg: "#f5f5f4", // stone-100
    tileBg: "#ffffff",
    headerBg: "#fafaf9", // stone-50
    sidebarBg: "#ffffff",
    titlebarBg: "#e7e5e4", // stone-200
    fgPrimary: "#1c1917", // stone-900
    fgMuted: "#78716c", // stone-500
    border: "#d6d3d1", // stone-300
    tileHeaderHeight: 24,
    showTileHeader: true,
    headerOnHover: false,
    showCwd: true,
    gridGap: 6,
    cornerRadius: 8,
    fontFamily:
      'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
    fontSize: 12,
    dotStarting: "#d97706",
    dotLive: "#16a34a",
    dotDetached: "#a8a29e",
    dotExited: "#d6d3d1",
    dotError: "#dc2626",
  },
  terminal: {
    background: "#ffffff",
    foreground: "#1c1917",
    cursor: "#2563eb",
    selection: "#bfdbfe",
    ansi: {
      black: "#1c1917",
      red: "#dc2626",
      green: "#16a34a",
      yellow: "#ca8a04",
      blue: "#2563eb",
      magenta: "#9333ea",
      cyan: "#0891b2",
      white: "#e7e5e4",
      brightBlack: "#78716c",
      brightRed: "#ef4444",
      brightGreen: "#22c55e",
      brightYellow: "#eab308",
      brightBlue: "#3b82f6",
      brightMagenta: "#a855f7",
      brightCyan: "#06b6d4",
      brightWhite: "#1c1917",
    },
  },
};

/** The built-in presets, in dropdown order. "Midnight" is the default. */
export const BUILTIN_PRESETS: Theme[] = [MIDNIGHT, SLATE, PAPER];

/** The default active theme on a fresh install. */
export const DEFAULT_THEME: Theme = MIDNIGHT;

// ---------------------------------------------------------------------------
// Apply: tokens -> CSS custom properties on :root (the instant-rerender step)
// ---------------------------------------------------------------------------

/**
 * Write every token of `theme` onto `:root` as a CSS custom property, so the
 * whole UI re-renders against the new values with no reload. Numeric length
 * tokens get a `px` suffix; the two header-visibility flags are translated into
 * the forms the components actually consume (a `display` value + 0/1 flags that
 * Tile mirrors onto data attributes). Idempotent and cheap — safe to call on
 * every token edit from the editor.
 */
export function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const c = theme.chrome;

  for (const key of Object.keys(CHROME_VAR) as (keyof ChromeTokens)[]) {
    const varName = CHROME_VAR[key];
    const value = c[key];
    // The three boolean tokens are consumed by Tile as data attributes (so the
    // stylesheet can own the header's height + hover-reveal); we still publish
    // them as 0/1 vars for any future CSS hook.
    if (
      key === "showTileHeader" ||
      key === "headerOnHover" ||
      key === "showCwd"
    ) {
      root.style.setProperty(varName, value ? "1" : "0");
      continue;
    }
    if (PX_KEYS.has(key)) {
      root.style.setProperty(varName, `${value as number}px`);
      continue;
    }
    root.style.setProperty(varName, String(value));
  }

  // Terminal palette tokens (optional). We publish them as vars too so any
  // future CSS hook can use them; Terminal.tsx owns the actual xterm ITheme and
  // is out of this workstream's scope, so we deliberately do not touch it here.
  const t = theme.terminal;
  if (t) {
    root.style.setProperty("--th-term-bg", t.background);
    root.style.setProperty("--th-term-fg", t.foreground);
    root.style.setProperty("--th-term-cursor", t.cursor);
  }
}

// ---------------------------------------------------------------------------
// Persistence (localStorage) + safe merge
// ---------------------------------------------------------------------------

const PERSIST_KEY = "termhub.theme.v1";

interface PersistedThemes {
  active: Theme;
  /** User-saved presets, by name (built-ins are not stored). */
  presets: Record<string, Theme>;
}

/**
 * Deep-fill a possibly-partial / possibly-foreign theme object against the
 * default, so an imported JSON or an older persisted blob can omit keys and
 * still yield a complete, render-safe Theme. Unknown keys are dropped (we only
 * copy keys the default declares). This is what makes Import tolerant and what
 * keeps a schema bump from bricking a saved theme.
 */
export function normalizeTheme(input: unknown, name?: string): Theme {
  const base = DEFAULT_THEME;
  const obj = (input ?? {}) as Partial<Theme>;
  const inChrome = (obj.chrome ?? {}) as Partial<ChromeTokens>;

  const chrome = {} as ChromeTokens;
  for (const key of Object.keys(base.chrome) as (keyof ChromeTokens)[]) {
    const v = inChrome[key];
    // Accept a value only if it matches the default's type; else fall back.
    chrome[key] = (
      typeof v === typeof base.chrome[key] && v !== undefined
        ? v
        : base.chrome[key]
    ) as never;
  }

  let terminal: TerminalPalette | undefined;
  const inTerm = obj.terminal;
  if (inTerm && typeof inTerm === "object") {
    const bt = base.terminal!;
    const it = inTerm as Partial<TerminalPalette>;
    const ansiIn = (it.ansi ?? {}) as Partial<AnsiPalette>;
    const ansi = {} as AnsiPalette;
    for (const k of Object.keys(bt.ansi) as (keyof AnsiPalette)[]) {
      ansi[k] = typeof ansiIn[k] === "string" ? ansiIn[k]! : bt.ansi[k];
    }
    terminal = {
      background:
        typeof it.background === "string" ? it.background : bt.background,
      foreground:
        typeof it.foreground === "string" ? it.foreground : bt.foreground,
      cursor: typeof it.cursor === "string" ? it.cursor : bt.cursor,
      selection: typeof it.selection === "string" ? it.selection : bt.selection,
      ansi,
    };
  }

  return {
    name: name ?? (typeof obj.name === "string" ? obj.name : base.name),
    chrome,
    ...(terminal ? { terminal } : {}),
  };
}

function loadPersisted(): PersistedThemes {
  if (typeof localStorage === "undefined") {
    return { active: DEFAULT_THEME, presets: {} };
  }
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (!raw) return { active: DEFAULT_THEME, presets: {} };
    const parsed = JSON.parse(raw) as Partial<PersistedThemes>;
    const presets: Record<string, Theme> = {};
    for (const [k, v] of Object.entries(parsed.presets ?? {})) {
      presets[k] = normalizeTheme(v, k);
    }
    return {
      active: normalizeTheme(parsed.active),
      presets,
    };
  } catch {
    return { active: DEFAULT_THEME, presets: {} };
  }
}

function savePersisted(active: Theme, presets: Record<string, Theme>): void {
  if (typeof localStorage === "undefined") return;
  try {
    const payload: PersistedThemes = { active, presets };
    localStorage.setItem(PERSIST_KEY, JSON.stringify(payload));
  } catch {
    // localStorage full / unavailable — non-fatal; theme stays in memory.
  }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

interface ThemeStore {
  /** The live, applied theme. Editing it re-renders the UI instantly. */
  active: Theme;
  /** User-saved presets, by name (built-ins live in BUILTIN_PRESETS). */
  presets: Record<string, Theme>;

  /**
   * Replace the active theme wholesale (preset switch / import / MCP push) and
   * apply + persist it. `fromBackend` suppresses the echo back to the backend
   * so applying a `theme://changed` event doesn't loop.
   */
  setTheme: (theme: Theme, fromBackend?: boolean) => void;

  /** Patch a single chrome token live (the editor's per-control handler). */
  setChromeToken: <K extends keyof ChromeTokens>(
    key: K,
    value: ChromeTokens[K],
  ) => void;

  /** Patch the terminal palette (or seed it from the default if absent). */
  setTerminalToken: (patch: Partial<TerminalPalette>) => void;
  /** Patch a single ANSI color. */
  setAnsiColor: (key: keyof AnsiPalette, value: string) => void;

  /** Save the current active theme as a named preset. */
  saveAsPreset: (name: string) => void;
  /** Delete a user preset by name (built-ins can't be deleted). */
  deletePreset: (name: string) => void;
  /** Switch to a preset by name (built-in or user); no-op if unknown. */
  applyPreset: (name: string) => void;

  /** Export the active theme as pretty JSON (for the clipboard / a .json file). */
  exportJSON: () => string;
  /** Import a theme from a JSON string; returns an error message or null. */
  importJSON: (json: string) => string | null;

  /** Reset the active theme back to the Midnight default. */
  resetToDefault: () => void;
}

const initial = loadPersisted();

/**
 * Push the active theme to the backend (the MCP-writable copy) so a theme set
 * in the editor is what Claude/MCP reads via `get_theme`. Best-effort and lazy:
 * the import is dynamic so the store has no hard dependency on the IPC layer
 * (and unit/import contexts without Tauri don't blow up). Failures are swallowed
 * — outside a Tauri webview there is simply no backend.
 */
function pushToBackend(theme: Theme): void {
  void import("../ipc/theme")
    .then((m) => m.setThemeBackend(theme))
    .catch(() => {});
}

export const useTheme = create<ThemeStore>((set, get) => ({
  active: initial.active,
  presets: initial.presets,

  setTheme: (theme, fromBackend = false) => {
    applyTheme(theme);
    savePersisted(theme, get().presets);
    set({ active: theme });
    if (!fromBackend) pushToBackend(theme);
  },

  setChromeToken: (key, value) => {
    const next: Theme = {
      ...get().active,
      chrome: { ...get().active.chrome, [key]: value },
    };
    get().setTheme(next);
  },

  setTerminalToken: (patch) => {
    const cur = get().active;
    const base = cur.terminal ?? DEFAULT_THEME.terminal!;
    const next: Theme = { ...cur, terminal: { ...base, ...patch } };
    get().setTheme(next);
  },

  setAnsiColor: (key, value) => {
    const cur = get().active;
    const base = cur.terminal ?? DEFAULT_THEME.terminal!;
    const next: Theme = {
      ...cur,
      terminal: { ...base, ansi: { ...base.ansi, [key]: value } },
    };
    get().setTheme(next);
  },

  saveAsPreset: (name) => {
    const trimmed = name.trim();
    if (!trimmed) return;
    const theme: Theme = { ...get().active, name: trimmed };
    const presets = { ...get().presets, [trimmed]: theme };
    savePersisted(theme, presets);
    set({ presets, active: theme });
  },

  deletePreset: (name) => {
    const presets = { ...get().presets };
    delete presets[name];
    savePersisted(get().active, presets);
    set({ presets });
  },

  applyPreset: (name) => {
    const builtin = BUILTIN_PRESETS.find((p) => p.name === name);
    const preset = builtin ?? get().presets[name];
    if (!preset) return;
    get().setTheme(preset);
  },

  exportJSON: () => JSON.stringify(get().active, null, 2),

  importJSON: (json) => {
    try {
      const parsed = JSON.parse(json);
      const theme = normalizeTheme(parsed);
      get().setTheme(theme);
      return null;
    } catch (e) {
      return e instanceof Error ? e.message : "Invalid JSON";
    }
  },

  resetToDefault: () => get().setTheme(DEFAULT_THEME),
}));

// ---------------------------------------------------------------------------
// Boot: apply immediately, then reconcile with the backend + subscribe to MCP
// ---------------------------------------------------------------------------

/**
 * One-time wiring, called once from the ThemeProvider on mount:
 *   1. Apply the persisted/active theme to :root right away (no flash).
 *   2. Ask the backend for its persisted theme (the MCP copy). If present and
 *      different, adopt it (the backend is the cross-surface source of truth);
 *      otherwise seed the backend with our local active theme.
 *   3. Subscribe to `theme://changed` so a theme set via MCP / another window
 *      applies here live.
 * Returns an unsubscribe for the event listener.
 */
export async function initThemeBridge(): Promise<() => void> {
  // 1. Apply current immediately.
  applyTheme(useTheme.getState().active);

  let unlisten: (() => void) = () => {};
  try {
    const ipc = await import("../ipc/theme");

    // 3. Subscribe first so we never miss a change during the get round-trip.
    unlisten = await ipc.onThemeChanged((theme) => {
      // fromBackend=true: apply + persist locally but don't echo back.
      useTheme.getState().setTheme(normalizeTheme(theme), true);
    });

    // 2. Reconcile with the backend's persisted theme.
    const backendTheme = await ipc.getThemeBackend();
    if (backendTheme) {
      useTheme.getState().setTheme(normalizeTheme(backendTheme), true);
    } else {
      await ipc.setThemeBackend(useTheme.getState().active);
    }
  } catch {
    // No Tauri backend (e.g. plain web / test) — local theme already applied.
  }

  return () => {
    try {
      unlisten();
    } catch {
      // ignore
    }
  };
}

/** Re-export the event channel name for convenience. */
export { ThemeEvents };
