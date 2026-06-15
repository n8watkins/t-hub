// Pluggable file-tree icon themes.
//
// The Files tree (FileTree.tsx) and the compact panel tree (FilePanel.tsx) both
// render rows through <FileTypeIcon> / <FolderTypeIcon>, which pick an icon set
// from the user's `fileIconTheme` setting:
//
//   - "lucide"  the original minimal lucide-react glyphs (default), tinted per type
//   - "vscode"  the colorful VS Code "vscode-icons" set (Iconify, bundled offline)
//   - "seti"    the Seti UI set (muted, per-type colors)
//
// lucide renders synchronously. seti renders the inline SVG string the package
// ships, recolored from a fixed Seti palette. vscode-icons are drawn by Iconify
// from an offline collection lazy-loaded the first time that theme is active
// (~3.6MB of JSON — code-split so it never lands in the main bundle); until it
// loads, rows fall back to the lucide glyph so they never flash a blank box.

import { useEffect, useState } from "react";
import { Icon, addCollection } from "@iconify/react";
import vscodeIconsJs from "vscode-icons-js";
import { themeIcons as setiThemeIcons } from "seti-icons";
import {
  File,
  FileCode,
  FileCog,
  FileJson,
  FileText,
  Folder,
  FolderOpen,
  Image as ImageIcon,
  type LucideIcon,
} from "lucide-react";
import { useSettings } from "../store/settings";

export type FileIconThemeId = "lucide" | "vscode" | "seti";

/** The selectable icon themes, in picker order. */
export const FILE_ICON_THEMES: { id: FileIconThemeId; label: string }[] = [
  { id: "lucide", label: "Lucide (minimal)" },
  { id: "vscode", label: "VS Code (colorful)" },
  { id: "seti", label: "Seti (muted)" },
];

const ICON_PX = 14;

/** Read + validate the active file-icon theme (unknown values => lucide). */
function useFileIconTheme(): FileIconThemeId {
  const raw = useSettings((s) => s.fileIconTheme);
  return raw === "vscode" || raw === "seti" ? raw : "lucide";
}

// ---------------------------------------------------------------------------
// lucide theme — the original scheme, moved here verbatim so both trees share it
// ---------------------------------------------------------------------------

/** Lowercased extension of a filename (no dot), or "" if none. */
function extOf(name: string): string {
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf(".");
  return dot >= 0 ? lower.slice(dot + 1) : "";
}

/** True for conventional config / dotfiles (`.env`, `.env.*`, `.*rc`, and the
 *  usual config basenames) — these get the `FileCog` glyph. */
function isConfigName(name: string): boolean {
  const lower = name.toLowerCase();
  return (
    lower === ".env" ||
    lower.startsWith(".env.") ||
    lower.endsWith("rc") || // .npmrc, .babelrc, .zshrc, .bashrc, eslintrc, …
    lower.endsWith(".config.js") ||
    lower.endsWith(".config.ts") ||
    lower.endsWith(".config.mjs") ||
    lower.endsWith(".config.cjs") ||
    lower === "tsconfig.json" ||
    lower === "tauri.conf.json" ||
    lower === "dockerfile" ||
    lower === "makefile" ||
    lower === ".gitignore" ||
    lower === ".gitattributes" ||
    lower === ".editorconfig"
  );
}

const CODE_EXTS = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "go", "py", "sh", "bash", "zsh",
  "c", "h", "cpp", "hpp", "java", "rb", "php", "html", "css", "scss",
]);
const TEXT_EXTS = new Set(["md", "mdx", "markdown", "txt", "rst", "log"]);
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "svg", "gif", "webp", "ico", "bmp", "avif"]);
const DATA_EXTS = new Set(["toml", "yaml", "yml"]);

/** Pick the lucide icon component for a filename. Config/dotfiles first (so
 *  `.env` reads as config, not a generic file), then by extension. */
function fileIconFor(name: string): LucideIcon {
  if (isConfigName(name)) return FileCog;
  const ext = extOf(name);
  if (ext === "json") return FileJson;
  if (IMAGE_EXTS.has(ext)) return ImageIcon;
  if (CODE_EXTS.has(ext)) return FileCode;
  if (TEXT_EXTS.has(ext)) return FileText;
  if (DATA_EXTS.has(ext)) return FileText; // toml/yaml: doc-ish config
  return File;
}

/** Map a filename to a category tint for its lucide glyph. */
function fileIconColor(name: string): string {
  const lower = name.toLowerCase();
  if (
    lower === "package.json" ||
    lower === "cargo.toml" ||
    lower === "tsconfig.json" ||
    lower === "tauri.conf.json" ||
    lower === "dockerfile" ||
    lower === "makefile" ||
    lower.startsWith("readme") ||
    lower.startsWith("license") ||
    lower.startsWith("licence") ||
    lower.startsWith("changelog")
  ) {
    return "var(--th-accent)";
  }
  return EXT_TINT[extOf(name)] ?? "var(--th-fg-muted)";
}

/** Extension → tint. Muted, category-grouped hues; intentionally small. */
const EXT_TINT: Record<string, string> = {
  ts: "#4f9cf0", tsx: "#4f9cf0", js: "#e2b93d", jsx: "#e2b93d", mjs: "#e2b93d",
  cjs: "#e2b93d", rs: "#d08770", go: "#4dc4d6", html: "#e06c4f", css: "#56a3e0",
  scss: "#cf649a", json: "#d6a84d", toml: "#9aa0a6", yaml: "#9aa0a6",
  yml: "#9aa0a6", md: "#7fb37f", mdx: "#7fb37f", markdown: "#7fb37f",
  txt: "#9aa0a6", py: "#5a9fd4", sh: "#89c07a", png: "#b48ead", jpg: "#b48ead",
  jpeg: "#b48ead", gif: "#b48ead", svg: "#b48ead", webp: "#b48ead",
};

function LucideFileGlyph({ name }: { name: string }) {
  const Glyph = fileIconFor(name);
  return (
    <span
      className="flex w-3.5 shrink-0 items-center justify-center"
      style={{ color: fileIconColor(name) }}
      aria-hidden
    >
      <Glyph size={ICON_PX} strokeWidth={2} />
    </span>
  );
}

function LucideFolderGlyph({ open }: { open: boolean }) {
  const Glyph = open ? FolderOpen : Folder;
  return (
    <span
      className="flex w-3.5 shrink-0 items-center justify-center"
      style={{ color: "var(--th-accent)" }}
      aria-hidden
    >
      <Glyph size={ICON_PX} strokeWidth={2} />
    </span>
  );
}

// ---------------------------------------------------------------------------
// vscode-icons theme — Iconify, offline collection lazy-loaded on first use
// ---------------------------------------------------------------------------

let vscodeCollectionLoaded = false;
let vscodeCollectionPromise: Promise<void> | null = null;

function ensureVscodeCollection(): Promise<void> {
  if (vscodeCollectionLoaded) return Promise.resolve();
  if (!vscodeCollectionPromise) {
    vscodeCollectionPromise = import("@iconify-json/vscode-icons/icons.json")
      .then((m) => {
        addCollection(m.default as Parameters<typeof addCollection>[0]);
        vscodeCollectionLoaded = true;
      })
      .catch((err) => {
        vscodeCollectionPromise = null; // allow a retry next time the theme is used
        throw err;
      });
  }
  return vscodeCollectionPromise;
}

/** True once the offline vscode-icons collection is registered. While false,
 *  callers fall back to a lucide glyph so a row never shows a blank box. */
function useVscodeReady(active: boolean): boolean {
  const [ready, setReady] = useState(vscodeCollectionLoaded);
  useEffect(() => {
    if (!active || ready) return;
    let alive = true;
    ensureVscodeCollection()
      .then(() => alive && setReady(true))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [active, ready]);
  return ready;
}

/** vscode-icons-js returns names like `file_type_reactts.svg`; the Iconify set
 *  keys them as `file-type-reactts`. Strip the suffix and swap `_`→`-`. */
function toIconifyName(raw: string | undefined, fallback: string): string {
  return raw ? raw.replace(/\.svg$/, "").replace(/_/g, "-") : fallback;
}

function VscodeGlyph({ icon }: { icon: string }) {
  return (
    <span className="flex w-3.5 shrink-0 items-center justify-center" aria-hidden>
      <Icon icon={`vscode-icons:${icon}`} width={ICON_PX} height={ICON_PX} />
    </span>
  );
}

// ---------------------------------------------------------------------------
// seti theme — inline SVG strings, recolored from a fixed Seti palette
// ---------------------------------------------------------------------------

// seti-icons ships the glyphs but leaves the palette to the consumer: getIcon
// returns a color TOKEN; themeIcons(palette) resolves it to a hex. These are the
// canonical Seti UI hues.
const setiIconFor = setiThemeIcons({
  blue: "#519aba",
  grey: "#4d5a5e",
  "grey-light": "#6d8086",
  green: "#8dc149",
  orange: "#e37933",
  pink: "#f55385",
  purple: "#a074c4",
  red: "#cc3e44",
  white: "#d4d7d6",
  yellow: "#cbcb41",
  ignore: "#41535b",
});

function SetiGlyph({ file }: { file: string }) {
  const { svg, color } = setiIconFor(file);
  // The package's SVG carries no fill and no size; inject the resolved color +
  // a 1em box so it inherits the row's sizing. Markup is local + static.
  const html = svg.replace(
    /^<svg /,
    `<svg fill="${color}" width="${ICON_PX}" height="${ICON_PX}" `,
  );
  return (
    <span
      className="flex w-3.5 shrink-0 items-center justify-center"
      aria-hidden
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

// ---------------------------------------------------------------------------
// Public, theme-aware row icons (read the active theme from the settings store)
// ---------------------------------------------------------------------------

/** A file row's icon, in whichever theme is selected. */
export function FileTypeIcon({ name }: { name: string }) {
  const theme = useFileIconTheme();
  const vscodeReady = useVscodeReady(theme === "vscode");
  if (theme === "vscode" && vscodeReady) {
    return (
      <VscodeGlyph icon={toIconifyName(vscodeIconsJs.getIconForFile(name), "default-file")} />
    );
  }
  if (theme === "seti") return <SetiGlyph file={name} />;
  return <LucideFileGlyph name={name} />;
}

/** A folder row's icon. seti has no folder set, so non-vscode themes use the
 *  lucide folder (accent-tinted) — folders read fine across all themes. */
export function FolderTypeIcon({ name, open }: { name: string; open: boolean }) {
  const theme = useFileIconTheme();
  const vscodeReady = useVscodeReady(theme === "vscode");
  if (theme === "vscode" && vscodeReady) {
    const raw = open
      ? vscodeIconsJs.getIconForOpenFolder(name)
      : vscodeIconsJs.getIconForFolder(name);
    return (
      <VscodeGlyph
        icon={toIconifyName(raw, open ? "default-folder-opened" : "default-folder")}
      />
    );
  }
  return <LucideFolderGlyph open={open} />;
}
