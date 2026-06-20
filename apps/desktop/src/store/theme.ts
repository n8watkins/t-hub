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
    tileHeaderHeight: 52,
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
    tileHeaderHeight: 52,
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

/**
 * "Dracula" — the well-known purple-tinted dark scheme (draculatheme.com).
 * Chrome derives from the canonical palette; the terminal carries the full
 * 16-color ANSI set so terminal output matches the upstream theme.
 */
const DRACULA: Theme = {
  name: "Dracula",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#bd93f9", // purple
    focusRing: "#bd93f9",
    appBg: "#282a36", // background
    tileBg: "#21222c", // darker surface
    headerBg: "#282a36cc",
    sidebarBg: "#21222c",
    titlebarBg: "#282a36",
    fgPrimary: "#f8f8f2", // foreground
    fgMuted: "#6272a4", // comment
    border: "#44475a", // selection/current-line
    cornerRadius: 6,
    dotStarting: "#ffb86c", // orange
    dotLive: "#50fa7b", // green
    dotDetached: "#6272a4", // comment
    dotExited: "#44475a",
    dotError: "#ff5555", // red
  },
  terminal: {
    background: "#282a36",
    foreground: "#f8f8f2",
    cursor: "#f8f8f2",
    selection: "#44475a",
    ansi: {
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
};

/**
 * "Nord" — the arctic, bluish palette (nordtheme.com). Polar-night surfaces,
 * snow-storm foreground, frost accent.
 */
const NORD: Theme = {
  name: "Nord",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#88c0d0", // frost
    focusRing: "#88c0d0",
    appBg: "#2e3440", // nord0 (polar night)
    tileBg: "#3b4252", // nord1
    headerBg: "#2e3440cc",
    sidebarBg: "#2e3440",
    titlebarBg: "#3b4252",
    fgPrimary: "#eceff4", // nord6 (snow storm)
    fgMuted: "#a9b3c9", // lightened frost so muted text clears AA on nord1 (was #7b88a1, 2.82:1)
    border: "#434c5e", // nord2
    cornerRadius: 6,
    dotStarting: "#ebcb8b", // aurora yellow
    dotLive: "#a3be8c", // aurora green
    dotDetached: "#81a1c1", // frost blue
    dotExited: "#4c566a", // nord3
    dotError: "#bf616a", // aurora red
  },
  terminal: {
    background: "#2e3440",
    foreground: "#d8dee9",
    cursor: "#d8dee9",
    selection: "#434c5e",
    ansi: {
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#4c566a",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
};

/**
 * "Solarized Dark" — Ethan Schoonover's precision palette (the dark base03
 * variant). Low-saturation accents on a teal-tinted dark ground.
 */
const SOLARIZED_DARK: Theme = {
  name: "Solarized Dark",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#268bd2", // blue
    focusRing: "#268bd2",
    appBg: "#002b36", // base03
    tileBg: "#073642", // base02
    headerBg: "#002b36cc",
    sidebarBg: "#002b36",
    titlebarBg: "#073642",
    fgPrimary: "#93a1a1", // base1
    fgMuted: "#839496", // base0 — muted text clears AA on base02 (was base01 #586e75, 2.42:1)
    border: "#073642", // base02
    cornerRadius: 4,
    dotStarting: "#b58900", // yellow
    dotLive: "#859900", // green
    dotDetached: "#657b83", // base00
    dotExited: "#073642",
    dotError: "#dc322f", // red
  },
  terminal: {
    background: "#002b36",
    foreground: "#839496", // base0
    cursor: "#93a1a1",
    selection: "#073642",
    ansi: {
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#002b36",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
};

/**
 * "Gruvbox Dark" — the warm, retro-groove palette (morhetz/gruvbox), dark
 * medium variant. Earthy backgrounds with bright aqua/orange accents.
 */
const GRUVBOX_DARK: Theme = {
  name: "Gruvbox Dark",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#fe8019", // orange
    focusRing: "#fe8019",
    appBg: "#282828", // bg0
    tileBg: "#3c3836", // bg1
    headerBg: "#282828cc",
    sidebarBg: "#282828",
    titlebarBg: "#3c3836",
    fgPrimary: "#ebdbb2", // fg1
    fgMuted: "#928374", // gray
    border: "#504945", // bg2
    cornerRadius: 4,
    dotStarting: "#fabd2f", // yellow
    dotLive: "#b8bb26", // green
    dotDetached: "#928374", // gray
    dotExited: "#504945",
    dotError: "#fb4934", // red
  },
  terminal: {
    background: "#282828",
    foreground: "#ebdbb2",
    cursor: "#ebdbb2",
    selection: "#504945",
    ansi: {
      black: "#282828",
      red: "#cc241d",
      green: "#98971a",
      yellow: "#d79921",
      blue: "#458588",
      magenta: "#b16286",
      cyan: "#689d6a",
      white: "#a89984",
      brightBlack: "#928374",
      brightRed: "#fb4934",
      brightGreen: "#b8bb26",
      brightYellow: "#fabd2f",
      brightBlue: "#83a598",
      brightMagenta: "#d3869b",
      brightCyan: "#8ec07c",
      brightWhite: "#ebdbb2",
    },
  },
};

/**
 * "High Contrast" — a maximum-legibility dark theme: pure black ground, pure
 * white text, saturated primaries, and a thick yellow focus ring. Useful for
 * accessibility and bright-room visibility.
 */
const HIGH_CONTRAST: Theme = {
  name: "High Contrast",
  chrome: {
    ...MIDNIGHT.chrome,
    accent: "#ffff00", // yellow
    focusRing: "#ffff00",
    focusRingWidth: 3,
    appBg: "#000000",
    tileBg: "#000000",
    headerBg: "#000000",
    sidebarBg: "#000000",
    titlebarBg: "#000000",
    fgPrimary: "#ffffff",
    fgMuted: "#c0c0c0",
    border: "#ffffff",
    cornerRadius: 0,
    dotStarting: "#ffff00",
    dotLive: "#00ff00",
    dotDetached: "#00ffff",
    dotExited: "#808080",
    dotError: "#ff0000",
  },
  terminal: {
    background: "#000000",
    foreground: "#ffffff",
    cursor: "#ffff00",
    selection: "#ffffff",
    ansi: {
      black: "#000000",
      red: "#ff0000",
      green: "#00ff00",
      yellow: "#ffff00",
      blue: "#0080ff",
      magenta: "#ff00ff",
      cyan: "#00ffff",
      white: "#ffffff",
      brightBlack: "#808080",
      brightRed: "#ff5555",
      brightGreen: "#55ff55",
      brightYellow: "#ffff55",
      brightBlue: "#5599ff",
      brightMagenta: "#ff55ff",
      brightCyan: "#55ffff",
      brightWhite: "#ffffff",
    },
  },
};

/** The built-in presets, in dropdown order. "Midnight" is the default. */
export const BUILTIN_PRESETS: Theme[] = [
  MIDNIGHT,
  SLATE,
  PAPER,
  DRACULA,
  NORD,
  SOLARIZED_DARK,
  GRUVBOX_DARK,
  HIGH_CONTRAST,
];

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
/** True when a hex color reads as a LIGHT surface. Parses #rgb / #rrggbb(/aa)
 *  (alpha ignored) and thresholds perceptual luminance. */
function isLightColor(hex: string): boolean {
  const h = hex.replace("#", "");
  const full =
    h.length === 3
      ? h
          .split("")
          .map((ch) => ch + ch)
          .join("")
      : h;
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  if ([r, g, b].some(Number.isNaN)) return false;
  return (0.299 * r + 0.587 * g + 0.114 * b) / 255 > 0.6;
}

export function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const c = theme.chrome;

  // Tell the engine which scheme to paint NATIVE controls in — the <select>
  // option popup, scrollbars, etc. Without this, WebView2 draws the native
  // dropdown list on a WHITE background while our <option> text is near-white,
  // i.e. unreadable white-on-white in every dark theme. Derive from the app
  // background's luminance so custom themes are handled too (Paper => light).
  root.style.colorScheme = isLightColor(c.appBg) ? "light" : "dark";

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
      active: migrateTileHeader(normalizeTheme(parsed.active)),
      presets,
    };
  } catch {
    return { active: DEFAULT_THEME, presets: {} };
  }
}

/**
 * One-time nudge: the default tile-header height has grown over time (22/24px →
 * 40px → now 52px) as the header gained a tab bar + controls and needed more
 * breathing room. A theme persisted before a bump keeps its old value via
 * normalizeTheme, so a user who never deliberately set the header height would be
 * stuck at the old default. Bump ONLY those legacy default values to the current
 * default; a height the user actually chose (anything else) is left untouched.
 */
function migrateTileHeader(t: Theme): Theme {
  const h = t.chrome.tileHeaderHeight;
  if (h === 22 || h === 24 || h === 40) {
    return {
      ...t,
      chrome: { ...t.chrome, tileHeaderHeight: DEFAULT_THEME.chrome.tileHeaderHeight },
    };
  }
  return t;
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
  /** Restore the active theme's terminal palette to the default (ANSI + base). */
  resetTerminalPalette: () => void;

  /** Per-terminal palette overrides, keyed by terminalId (sparse patches that
   *  win over the active theme's terminal palette for that one terminal). */
  termOverrides: Record<string, Partial<TerminalPalette>>;
  /** Merge a palette patch into a single terminal's override. */
  setTermOverride: (id: string, patch: Partial<TerminalPalette>) => void;
  /** Drop a terminal's override so it follows the global theme again. */
  clearTermOverride: (id: string) => void;

  /** Per-terminal focus-ring color (terminalId → color); falls back to the
   *  global --th-focus-ring when unset. */
  termFocusRing: Record<string, string>;
  setTermFocusRing: (id: string, color: string) => void;
  clearTermFocusRing: (id: string) => void;

  /** "What are you working on" name, keyed by the terminal's CWD (project path) —
   *  NOT the ephemeral terminal id — so it's durable: it shows in the tile header,
   *  the sidebar Workspaces list, AND the Recent row for that project, and it
   *  survives relaunch/resume. Free-text + cosmetic ("name this work…"); NOT a
   *  branch and NOT the tab/derived label. Own localStorage slot. */
  workNames: Record<string, string>;
  setWorkName: (cwd: string, name: string) => void;
  clearWorkName: (cwd: string) => void;

  /** Per-workspace color identity (tabId → color). A workspace's color cascades
   *  to its tiles (the tile focus ring, sidebar accent, tab dot). A per-terminal
   *  override (termFocusRing) still LAYERS ON TOP; the workspace color only beats
   *  the global blue default. Persisted in its own localStorage slot, mirroring
   *  the per-terminal override pattern. */
  workspaceColors: Record<string, string>;
  setWorkspaceColor: (tabId: string, color: string) => void;
  clearWorkspaceColor: (tabId: string) => void;

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

// ---------------------------------------------------------------------------
// Per-terminal palette overrides
//
// A terminal can override the *global* theme's terminal palette with its own
// colors (set from the tile's ⋯ menu) so the user can visually tell apart what
// they're working on. Stored as a SPARSE patch per terminalId — only the keys
// the user actually changed — merged over the active theme at render time.
// Keyed by terminalId, persisted in its own localStorage slot (kept out of the
// theme/preset blob). Cleared when the terminal is removed (see workspace's
// cleanupTileSideState) so a recycled id can't inherit stale colors.
// ---------------------------------------------------------------------------
const TERM_OVERRIDES_KEY = "termhub.theme.termOverrides";

function loadTermOverrides(): Record<string, Partial<TerminalPalette>> {
  try {
    if (typeof localStorage === "undefined") return {};
    const raw = localStorage.getItem(TERM_OVERRIDES_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, Partial<TerminalPalette>>)
      : {};
  } catch {
    return {};
  }
}

function saveTermOverrides(m: Record<string, Partial<TerminalPalette>>): void {
  try {
    localStorage.setItem(TERM_OVERRIDES_KEY, JSON.stringify(m));
  } catch {
    /* ignore */
  }
}

// Per-terminal focus-ring color (terminalId → color), in its own slot. Falls
// back to the global --th-focus-ring when a terminal has no override.
const TERM_FOCUS_KEY = "termhub.theme.termFocusRing";
function loadTermFocusRing(): Record<string, string> {
  try {
    if (typeof localStorage === "undefined") return {};
    const raw = localStorage.getItem(TERM_FOCUS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, string>)
      : {};
  } catch {
    return {};
  }
}
function saveTermFocusRing(m: Record<string, string>): void {
  try {
    localStorage.setItem(TERM_FOCUS_KEY, JSON.stringify(m));
  } catch {
    /* ignore */
  }
}

// Work name keyed by CWD (project path → name), in its own slot. Keyed by cwd
// (not terminal id) so it's durable across spawns/resumes and can surface on the
// project's Recent row. Same sparse-map-in-localStorage shape.
const WORKNAME_KEY = "termhub.theme.workNames";
function loadWorkNames(): Record<string, string> {
  try {
    if (typeof localStorage === "undefined") return {};
    const raw = localStorage.getItem(WORKNAME_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, string>)
      : {};
  } catch {
    return {};
  }
}
function saveWorkNames(m: Record<string, string>): void {
  try {
    localStorage.setItem(WORKNAME_KEY, JSON.stringify(m));
  } catch {
    /* ignore */
  }
}

// Per-workspace color (tabId → color), in its own slot. A workspace's color is
// the per-tab identity that cascades to its tiles (focus ring / sidebar accent /
// tab dot). Same sparse-map-in-localStorage shape as the per-terminal overrides.
const WORKSPACE_COLORS_KEY = "termhub.theme.workspaceColors";
function loadWorkspaceColors(): Record<string, string> {
  try {
    if (typeof localStorage === "undefined") return {};
    const raw = localStorage.getItem(WORKSPACE_COLORS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, string>)
      : {};
  } catch {
    return {};
  }
}
function saveWorkspaceColors(m: Record<string, string>): void {
  try {
    localStorage.setItem(WORKSPACE_COLORS_KEY, JSON.stringify(m));
  } catch {
    /* ignore */
  }
}

/**
 * A small, tasteful palette of workspace colors offered in the color picker
 * (the workspace tab's ⋯ menu / sidebar dot). Picked to read clearly on the dark
 * chrome and to be distinguishable from one another. Free-form colors are also
 * allowed (the `<input type="color">` swatch), so this is just the quick menu.
 */
export const WORKSPACE_COLOR_PALETTE: readonly string[] = [
  "#38bdf8", // sky
  "#34d399", // emerald
  "#a78bfa", // violet
  "#f472b6", // pink
  "#fbbf24", // amber
  "#fb7185", // rose
  "#22d3ee", // cyan
  "#a3e635", // lime
];

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
  termOverrides: loadTermOverrides(),
  termFocusRing: loadTermFocusRing(),
  workNames: loadWorkNames(),
  workspaceColors: loadWorkspaceColors(),

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

  resetTerminalPalette: () => {
    const cur = get().active;
    // Restore the active theme's terminal palette to the default (deep-cloned so
    // later edits don't mutate DEFAULT_THEME). Routes through setTheme so it
    // applies + persists + echoes to the backend like every other edit.
    const def = DEFAULT_THEME.terminal!;
    const next: Theme = {
      ...cur,
      terminal: { ...def, ansi: { ...def.ansi } },
    };
    get().setTheme(next);
  },

  setTermOverride: (id, patch) => {
    const cur = get().termOverrides[id] ?? {};
    const next = { ...get().termOverrides, [id]: { ...cur, ...patch } };
    saveTermOverrides(next);
    set({ termOverrides: next });
  },

  clearTermOverride: (id) => {
    if (!(id in get().termOverrides)) return;
    const next = { ...get().termOverrides };
    delete next[id];
    saveTermOverrides(next);
    set({ termOverrides: next });
  },

  setTermFocusRing: (id, color) => {
    const next = { ...get().termFocusRing, [id]: color };
    saveTermFocusRing(next);
    set({ termFocusRing: next });
  },

  clearTermFocusRing: (id) => {
    if (!(id in get().termFocusRing)) return;
    const next = { ...get().termFocusRing };
    delete next[id];
    saveTermFocusRing(next);
    set({ termFocusRing: next });
  },

  setWorkName: (cwd, name) => {
    if (!cwd) return;
    const trimmed = name.trim();
    // A blank value clears the slot (back to the placeholder); no-op if unchanged.
    if (!trimmed) {
      get().clearWorkName(cwd);
      return;
    }
    if (get().workNames[cwd] === trimmed) return;
    const next = { ...get().workNames, [cwd]: trimmed };
    saveWorkNames(next);
    set({ workNames: next });
  },

  clearWorkName: (cwd) => {
    if (!(cwd in get().workNames)) return;
    const next = { ...get().workNames };
    delete next[cwd];
    saveWorkNames(next);
    set({ workNames: next });
  },

  setWorkspaceColor: (tabId, color) => {
    const next = { ...get().workspaceColors, [tabId]: color };
    saveWorkspaceColors(next);
    set({ workspaceColors: next });
  },

  clearWorkspaceColor: (tabId) => {
    if (!(tabId in get().workspaceColors)) return;
    const next = { ...get().workspaceColors };
    delete next[tabId];
    saveWorkspaceColors(next);
    set({ workspaceColors: next });
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
