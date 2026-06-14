// ThemeEditor — the live Settings surface the app was missing (PRD §5.5).
//
// The whole user-facing promise of the theming system lives here: a person
// customizes TermHub's look WITHOUT editing config files, and every change is
// instant (each control writes a token into the theme store, which writes a CSS
// var, which re-renders the chrome). It is a fully self-contained overlay:
//   - Its open/closed state lives in the settings store (so other surfaces can
//     open it too); a global `Ctrl/Cmd+,` keydown listener toggles it via the
//     store, so App.tsx never has to know it exists.
//   - It renders nothing until opened (a fixed, centered panel + scrim).
//   - Esc closes it; a click on the scrim closes it.
//
// Structure: the panel has a left nav splitting settings into two top-level
// sections — **General** (app behavior flags from useSettings) and **Theme**
// (presets, colors, status dots, layout, typography, terminal palette). Each
// control is bound straight to a store's setters, so editing is live. The Theme
// section's Preset group also does Save-as-preset, preset switching, and
// Import/Export JSON (themes are shareable text, like VS Code).
import { useEffect, useRef, useState } from "react";
import {
  useTheme,
  BUILTIN_PRESETS,
  type ChromeTokens,
  type AnsiPalette,
} from "../store/theme";
import { useSettings } from "../store/settings";

/**
 * Wire the global `Ctrl/Cmd+,` toggle (and Esc-to-close) onto the settings
 * store, so the panel's open state is shared rather than component-local.
 */
function useEditorHotkeys(): void {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  const closeSettings = useSettings((s) => s.closeSettings);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      // Ctrl/Cmd+, opens/closes the panel (matches the conventional "settings"
      // shortcut). `e.key` is "," regardless of layout shifts for this combo.
      if (mod && e.key === "," && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        toggleSettings();
      } else if (e.key === "Escape") {
        // Only consume Escape when we're actually open (don't swallow it
        // globally — terminals/inputs may want it when the panel is closed).
        if (useSettings.getState().settingsOpen) {
          e.preventDefault();
          closeSettings();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggleSettings, closeSettings]);
}

export function ThemeEditor() {
  useEditorHotkeys();
  const open = useSettings((s) => s.settingsOpen);
  const closeSettings = useSettings((s) => s.closeSettings);
  if (!open) return null;
  return <ThemeEditorPanel onClose={closeSettings} />;
}

// ---------------------------------------------------------------------------
// The panel (only mounted while open).
// ---------------------------------------------------------------------------
type SectionId = "general" | "theme";

function ThemeEditorPanel({ onClose }: { onClose: () => void }) {
  const [section, setSection] = useState<SectionId>("general");

  return (
    // Scrim: a click anywhere outside the panel closes the editor. The panel
    // itself stops propagation so inner clicks don't bubble to the scrim.
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      onMouseDown={onClose}
      // The bootstrap host is pointer-events:none (so it's inert when closed);
      // re-enable events on the open overlay so the scrim + panel are clickable.
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex max-h-[85vh] w-[680px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        {/* Header */}
        <div
          className="flex shrink-0 items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div className="text-sm font-semibold">Settings</div>
          <button
            type="button"
            onClick={onClose}
            className="rounded px-1.5 leading-none hover:bg-neutral-700/40"
            title="Close (Esc · Ctrl/Cmd+,)"
            aria-label="Close settings"
            style={{ color: "var(--th-fg-muted)" }}
          >
            ×
          </button>
        </div>

        {/* Body: left nav (top-level sections) + scrollable content pane. */}
        <div className="flex min-h-0 flex-1">
          <SectionNav active={section} onSelect={setSection} />
          <div className="th-scroll min-h-0 flex-1 overflow-y-auto px-4 py-3">
            {section === "general" ? <GeneralSection /> : <ThemeSection />}
          </div>
        </div>
      </div>
    </div>
  );
}

/** The left-hand nav switching between the top-level settings sections. */
function SectionNav({
  active,
  onSelect,
}: {
  active: SectionId;
  onSelect: (s: SectionId) => void;
}) {
  const items: { id: SectionId; label: string; hint: string }[] = [
    { id: "general", label: "General", hint: "App behavior" },
    { id: "theme", label: "Theme", hint: "Colors & layout" },
  ];
  return (
    <nav
      className="flex w-36 shrink-0 flex-col gap-0.5 border-r p-2"
      style={{ borderColor: "var(--th-border)" }}
      aria-label="Settings sections"
    >
      {items.map((it) => {
        const isActive = it.id === active;
        return (
          <button
            key={it.id}
            type="button"
            onClick={() => onSelect(it.id)}
            className="rounded px-2 py-1.5 text-left text-xs transition-colors hover:bg-neutral-700/30"
            aria-current={isActive ? "page" : undefined}
            title={it.hint}
            style={{
              backgroundColor: isActive ? "var(--th-tile-bg)" : "transparent",
              color: isActive ? "var(--th-fg)" : "var(--th-fg-muted)",
              fontWeight: isActive ? 600 : 400,
            }}
          >
            {it.label}
          </button>
        );
      })}
    </nav>
  );
}

// ---------------------------------------------------------------------------
// General section — app behavior flags (settings store, not theme tokens).
// ---------------------------------------------------------------------------
function GeneralSection() {
  const revealPushesContent = useSettings((s) => s.revealPushesContent);
  const setRevealPushesContent = useSettings((s) => s.setRevealPushesContent);
  const autoHideTitlebarMaximized = useSettings(
    (s) => s.autoHideTitlebarMaximized,
  );
  const setAutoHideTitlebarMaximized = useSettings(
    (s) => s.setAutoHideTitlebarMaximized,
  );

  return (
    <Group title="Titlebar">
      <SettingToggleRow
        label="Auto-hide titlebar when maximized"
        hint="When the window is maximized, hide the titlebar to reclaim space. It reappears when you move the pointer to the top edge."
        value={autoHideTitlebarMaximized}
        onChange={setAutoHideTitlebarMaximized}
      />
      <SettingToggleRow
        label="Titlebar reveal pushes content down"
        hint="When a hidden titlebar reveals, push the content down to make room (on) instead of overlaying it on top of the content (off)."
        value={revealPushesContent}
        onChange={setRevealPushesContent}
      />
    </Group>
  );
}

// ---------------------------------------------------------------------------
// Theme section — presets, colors, status dots, layout, type, terminal palette.
// ---------------------------------------------------------------------------
function ThemeSection() {
  const active = useTheme((s) => s.active);
  const presets = useTheme((s) => s.presets);
  const setChromeToken = useTheme((s) => s.setChromeToken);
  const applyPreset = useTheme((s) => s.applyPreset);
  const saveAsPreset = useTheme((s) => s.saveAsPreset);
  const deletePreset = useTheme((s) => s.deletePreset);
  const exportJSON = useTheme((s) => s.exportJSON);
  const importJSON = useTheme((s) => s.importJSON);
  const resetToDefault = useTheme((s) => s.resetToDefault);

  const c = active.chrome;
  const presetNames = [
    ...BUILTIN_PRESETS.map((p) => p.name),
    ...Object.keys(presets),
  ];
  const isUserPreset = (name: string) =>
    Object.prototype.hasOwnProperty.call(presets, name);

  return (
    <>
      {/* --- Presets / share --- */}
      <Group title="Preset">
        <Row label="Active">
          <ThemeSelect
            value={presetNames.includes(active.name) ? active.name : ""}
            onChange={(v) => applyPreset(v)}
            title="Switch to a built-in or saved preset"
          >
            {!presetNames.includes(active.name) && (
              <Opt value="">{active.name} (edited)</Opt>
            )}
            {presetNames.map((n) => (
              <Opt key={n} value={n}>
                {n}
                {isUserPreset(n) ? " ·" : ""}
              </Opt>
            ))}
          </ThemeSelect>
        </Row>
        <PresetActions
          activeName={active.name}
          canDelete={isUserPreset(active.name)}
          onSave={saveAsPreset}
          onDelete={() => deletePreset(active.name)}
          onExport={exportJSON}
          onImport={importJSON}
          onReset={resetToDefault}
        />
      </Group>

      {/* --- Colors --- */}
      <Group title="Colors" cols={2}>
        <ColorRow label="Accent" k="accent" value={c.accent} set={setChromeToken} hint="Brand color: active tab dot, hover affordances, primary buttons." />
        <ColorRow label="Focus ring" k="focusRing" value={c.focusRing} set={setChromeToken} hint="Outline color drawn around the focused tile." />
        <ColorRow label="App background" k="appBg" value={c.appBg} set={setChromeToken} hint="The canvas backdrop behind all tiles." />
        <ColorRow label="Tile background" k="tileBg" value={c.tileBg} set={setChromeToken} hint="A tile's body, behind the terminal." />
        <ColorRow label="Header background" k="headerBg" value={c.headerBg} set={setChromeToken} hint="A tile header's background (supports 8-digit alpha hex)." />
        <ColorRow label="Sidebar background" k="sidebarBg" value={c.sidebarBg} set={setChromeToken} hint="The sidebar surface background." />
        <ColorRow label="Titlebar background" k="titlebarBg" value={c.titlebarBg} set={setChromeToken} hint="The top titlebar background." />
        <ColorRow label="Border" k="border" value={c.border} set={setChromeToken} hint="Hairline border color used across tiles, headers, and the sidebar." />
        <ColorRow label="Text" k="fgPrimary" value={c.fgPrimary} set={setChromeToken} hint="Primary text color." />
        <ColorRow label="Muted text" k="fgMuted" value={c.fgMuted} set={setChromeToken} hint="Secondary/dimmed text (cwd, captions)." />
      </Group>

      {/* --- Status dots --- */}
      <Group
        title="Status dots"
        cols={2}
        description="The colored dot shown per terminal lifecycle state."
      >
        <ColorRow label="Starting" k="dotStarting" value={c.dotStarting} set={setChromeToken} hint="A terminal that is starting up." />
        <ColorRow label="Live" k="dotLive" value={c.dotLive} set={setChromeToken} hint="A running, attached terminal." />
        <ColorRow label="Detached" k="dotDetached" value={c.dotDetached} set={setChromeToken} hint="A live session with no attached view." />
        <ColorRow label="Exited" k="dotExited" value={c.dotExited} set={setChromeToken} hint="A terminal whose process has exited." />
        <ColorRow label="Error" k="dotError" value={c.dotError} set={setChromeToken} hint="A terminal that failed to start or crashed." />
      </Group>

      {/* --- Layout --- */}
      <Group title="Layout">
        <SliderRow
          label="Tile header height"
          hint="Height of the header bar at the top of each tile (px)."
          k="tileHeaderHeight"
          value={c.tileHeaderHeight}
          min={16}
          max={40}
          suffix="px"
          set={setChromeToken}
        />
        <SliderRow
          label="Focus ring width"
          hint="Thickness of the outline around the focused tile (px). 0 disables it."
          k="focusRingWidth"
          value={c.focusRingWidth}
          min={0}
          max={4}
          suffix="px"
          set={setChromeToken}
        />
        <SliderRow
          label="Grid gap"
          hint="Spacing between tiles in the grid (px)."
          k="gridGap"
          value={c.gridGap}
          min={0}
          max={24}
          suffix="px"
          set={setChromeToken}
        />
        <SliderRow
          label="Corner radius"
          hint="Roundness of tile and chrome corners (px). 0 is square."
          k="cornerRadius"
          value={c.cornerRadius}
          min={0}
          max={20}
          suffix="px"
          set={setChromeToken}
        />
        <ToggleRow
          label="Show tile header"
          hint="Show the header bar (title, status, controls) on each tile."
          k="showTileHeader"
          value={c.showTileHeader}
          set={setChromeToken}
        />
        <ToggleRow
          label="Header on hover only"
          hint="Hide the tile header until you hover the tile, for a compact look."
          k="headerOnHover"
          value={c.headerOnHover}
          set={setChromeToken}
        />
        <ToggleRow
          label="Show cwd"
          hint="Show the terminal's current working directory in the tile header."
          k="showCwd"
          value={c.showCwd}
          set={setChromeToken}
        />
      </Group>

      {/* --- Typography --- */}
      <Group title="Typography">
        <Row label="UI font">
          <ThemeSelect
            value={c.fontFamily}
            onChange={(v) => setChromeToken("fontFamily", v)}
            title="Font family used across the app chrome"
          >
            {FONT_OPTIONS.map((f) => (
              <Opt key={f.label} value={f.value}>
                {f.label}
              </Opt>
            ))}
          </ThemeSelect>
        </Row>
        <SliderRow
          label="Base font size"
          hint="Base UI font size for the app chrome (px)."
          k="fontSize"
          value={c.fontSize}
          min={9}
          max={18}
          suffix="px"
          set={setChromeToken}
        />
      </Group>

      {/* --- Terminal palette --- */}
      <TerminalGroup />
    </>
  );
}

// ---------------------------------------------------------------------------
// Preset actions (save / delete / import / export / reset).
// ---------------------------------------------------------------------------
function PresetActions({
  activeName,
  canDelete,
  onSave,
  onDelete,
  onExport,
  onImport,
  onReset,
}: {
  activeName: string;
  canDelete: boolean;
  onSave: (name: string) => void;
  onDelete: () => void;
  onExport: () => string;
  onImport: (json: string) => string | null;
  onReset: () => void;
}) {
  const [name, setName] = useState(activeName);
  const [msg, setMsg] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  const flash = (m: string) => {
    setMsg(m);
    window.setTimeout(() => setMsg(null), 2000);
  };

  const doExport = async () => {
    const json = onExport();
    try {
      await navigator.clipboard.writeText(json);
      flash("Copied JSON to clipboard");
    } catch {
      // Clipboard blocked — fall back to a download so export still works.
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `${activeName || "theme"}.json`;
      a.click();
      URL.revokeObjectURL(url);
      flash("Downloaded theme.json");
    }
  };

  const onFile = async (file: File) => {
    const text = await file.text();
    const err = onImport(text);
    flash(err ? `Import failed: ${err}` : "Imported");
  };

  return (
    <div className="mt-1 flex flex-col gap-2">
      <div className="flex gap-1.5">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Preset name"
          className="min-w-0 flex-1 rounded border bg-transparent px-2 py-1 text-xs"
          style={{ borderColor: "var(--th-border)" }}
        />
        <Btn onClick={() => onSave(name)} title="Save the current theme as a named preset">
          Save
        </Btn>
        {canDelete && (
          <Btn onClick={onDelete} title="Delete this user preset">
            Delete
          </Btn>
        )}
      </div>
      <div className="flex gap-1.5">
        <Btn onClick={doExport} title="Copy the active theme as JSON to the clipboard">
          Export
        </Btn>
        <Btn onClick={() => fileRef.current?.click()} title="Load a theme from a JSON file">
          Import
        </Btn>
        <Btn onClick={onReset} title="Reset to the Midnight default">
          Reset
        </Btn>
        <input
          ref={fileRef}
          type="file"
          accept="application/json,.json"
          className="hidden"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void onFile(f);
            e.target.value = "";
          }}
        />
      </div>
      {msg && (
        <div className="text-[10px]" style={{ color: "var(--th-fg-muted)" }}>
          {msg}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Terminal palette group (optional palette; xterm ITheme is applied elsewhere).
// ---------------------------------------------------------------------------
function TerminalGroup() {
  const active = useTheme((s) => s.active);
  const setTerminalToken = useTheme((s) => s.setTerminalToken);
  const setAnsiColor = useTheme((s) => s.setAnsiColor);
  const term = active.terminal;
  if (!term) return null;

  const ansiKeys = Object.keys(term.ansi) as (keyof AnsiPalette)[];
  return (
    <Group
      title="Terminal palette"
      description="Colors used inside terminals (xterm). Background, foreground, cursor, and selection apply to all terminal output."
    >
      <ColorInputRow
        label="Background"
        value={term.background}
        onChange={(v) => setTerminalToken({ background: v })}
        hint="Terminal background color."
      />
      <ColorInputRow
        label="Foreground"
        value={term.foreground}
        onChange={(v) => setTerminalToken({ foreground: v })}
        hint="Default terminal text color."
      />
      <ColorInputRow
        label="Cursor"
        value={term.cursor}
        onChange={(v) => setTerminalToken({ cursor: v })}
        hint="Terminal cursor color."
      />
      <ColorInputRow
        label="Selection"
        value={term.selection}
        onChange={(v) => setTerminalToken({ selection: v })}
        hint="Highlight color for selected terminal text."
      />

      {/* ANSI palette: the 16 fixed slots terminal programs draw with. The slot
          names (black, red, ...) are not editable — only the colors are. */}
      <div className="mt-3">
        <div
          className="text-[10px] font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          ANSI palette
        </div>
        <p className="mt-0.5 text-[10px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
          The 16 base colors terminal programs use. The slot names (black, red,
          …) are fixed ANSI roles — only their colors are editable.
        </p>
        <div className="mt-1.5 grid grid-cols-2 gap-x-3">
          {ansiKeys.map((k) => (
            <ColorInputRow
              key={k}
              label={k}
              value={term.ansi[k]}
              onChange={(v) => setAnsiColor(k, v)}
              labelTitle={`ANSI "${k}" slot — a fixed role name; only its color is editable`}
            />
          ))}
        </div>
      </div>
    </Group>
  );
}

// ---------------------------------------------------------------------------
// Small presentational primitives (kept inline for cohesion).
// ---------------------------------------------------------------------------
const FONT_OPTIONS: { label: string; value: string }[] = [
  {
    label: "System Sans",
    value:
      'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
  },
  {
    label: "Monospace",
    value:
      'ui-monospace, "Cascadia Code", "JetBrains Mono", Menlo, Consolas, monospace',
  },
  { label: "Georgia (serif)", value: 'Georgia, Cambria, "Times New Roman", serif' },
  { label: "Inter", value: 'Inter, ui-sans-serif, system-ui, sans-serif' },
];

function Group({
  title,
  description,
  children,
  cols = 1,
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
  cols?: 1 | 2;
}) {
  return (
    <section className="mb-4">
      <div
        className="text-[10px] font-semibold uppercase tracking-wide"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {title}
      </div>
      {description && (
        <p
          className="mb-1.5 mt-0.5 text-[10px] leading-snug"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {description}
        </p>
      )}
      <div
        className={
          (description ? "" : "mt-1.5 ") +
          (cols === 2
            ? "grid grid-cols-2 gap-x-4 gap-y-1.5"
            : "flex flex-col gap-1.5")
        }
      >
        {children}
      </div>
    </section>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 text-xs">
      <span className="shrink-0" style={{ color: "var(--th-fg)" }}>
        {label}
      </span>
      <span className="flex min-w-0 flex-1 justify-end">{children}</span>
    </div>
  );
}

/**
 * A themed <select>. Native option lists are OS-drawn, but we style the closed
 * control (bg/text/border/radius) with theme vars and best-effort the options.
 */
function ThemeSelect({
  value,
  onChange,
  title,
  children,
}: {
  value: string;
  onChange: (v: string) => void;
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      title={title}
      className="w-full cursor-pointer rounded border px-2 py-1 text-xs outline-none"
      style={{
        backgroundColor: "var(--th-tile-bg)",
        color: "var(--th-fg)",
        borderColor: "var(--th-border)",
        borderRadius: "var(--th-radius)",
      }}
    >
      {children}
    </select>
  );
}

/** A themed <option> (best-effort: native popups are OS-drawn). */
function Opt({ value, children }: { value: string; children: React.ReactNode }) {
  return (
    <option
      value={value}
      style={{ backgroundColor: "var(--th-tile-bg)", color: "var(--th-fg)" }}
    >
      {children}
    </option>
  );
}

/** A color control wired to a chrome token by key. */
function ColorRow({
  label,
  k,
  value,
  set,
  hint,
}: {
  label: string;
  k: keyof ChromeTokens;
  value: string;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
  hint?: string;
}) {
  return (
    <ColorInputRow
      label={label}
      value={value}
      onChange={(v) => set(k, v as ChromeTokens[typeof k])}
      hint={hint}
    />
  );
}

/**
 * A raw color control: a native swatch + a hex text input. Some tokens carry an
 * 8-digit (alpha) hex; the native picker can't show alpha, so we keep the text
 * field as the source of truth and only feed the picker the leading #rrggbb.
 *
 * IMPORTANT: only the swatch opens the native color picker. The label is plain
 * text (not a <label> wrapping the color input), so clicking the name does
 * nothing; the hex field stays independently editable.
 */
function ColorInputRow({
  label,
  value,
  onChange,
  hint,
  labelTitle,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  /** Tooltip shown on the whole row (explains the control). */
  hint?: string;
  /** Tooltip shown specifically on the label text (overrides `hint` there). */
  labelTitle?: string;
}) {
  const swatch = value.startsWith("#") ? value.slice(0, 7) : "#000000";
  return (
    <div className="flex items-center justify-between gap-2 text-xs" title={hint}>
      <span
        className="min-w-0 flex-1 truncate"
        style={{ color: "var(--th-fg)" }}
        title={labelTitle ?? hint}
      >
        {label}
      </span>
      <input
        type="color"
        value={swatch}
        onChange={(e) => onChange(e.target.value)}
        className="h-5 w-6 shrink-0 cursor-pointer rounded border-0 bg-transparent p-0"
        aria-label={`${label} color`}
        title={`Pick ${label} color`}
      />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        className="w-[88px] shrink-0 rounded border bg-transparent px-1.5 py-0.5 font-mono text-[11px]"
        style={{ borderColor: "var(--th-border)" }}
        aria-label={`${label} hex`}
        title={`${label} hex value`}
      />
    </div>
  );
}

/** A numeric slider wired to a chrome token by key. */
function SliderRow({
  label,
  k,
  value,
  min,
  max,
  suffix,
  set,
  hint,
}: {
  label: string;
  k: keyof ChromeTokens;
  value: number;
  min: number;
  max: number;
  suffix?: string;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
  hint?: string;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-xs" title={hint}>
      <span className="shrink-0" style={{ color: "var(--th-fg)" }} title={hint}>
        {label}
      </span>
      <div className="flex items-center gap-2">
        <input
          type="range"
          min={min}
          max={max}
          step={1}
          value={value}
          onChange={(e) => set(k, Number(e.target.value) as ChromeTokens[typeof k])}
          className="w-28 cursor-pointer"
          style={{ accentColor: "var(--th-accent)" }}
          title={hint}
        />
        <span
          className="w-10 text-right font-mono text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {value}
          {suffix}
        </span>
      </div>
    </div>
  );
}

/** A boolean toggle wired to a chrome token by key. */
function ToggleRow({
  label,
  k,
  value,
  set,
  hint,
}: {
  label: string;
  k: keyof ChromeTokens;
  value: boolean;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
  hint?: string;
}) {
  return (
    <label
      className="flex cursor-pointer items-center justify-between gap-3 text-xs"
      title={hint}
    >
      <span style={{ color: "var(--th-fg)" }}>{label}</span>
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => set(k, e.target.checked as ChromeTokens[typeof k])}
        className="h-3.5 w-3.5 cursor-pointer"
        style={{ accentColor: "var(--th-accent)" }}
      />
    </label>
  );
}

/**
 * A boolean toggle wired to a plain callback (used for settings-store flags,
 * which aren't chrome tokens). Same visual style as {@link ToggleRow}, with an
 * optional muted helper line under the label.
 */
function SettingToggleRow({
  label,
  value,
  onChange,
  hint,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
  hint?: string;
}) {
  return (
    <label
      className="flex cursor-pointer items-start justify-between gap-3 text-xs"
      title={hint}
    >
      <span className="flex min-w-0 flex-col">
        <span style={{ color: "var(--th-fg)" }}>{label}</span>
        {hint && (
          <span
            className="mt-0.5 text-[10px] leading-snug"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {hint}
          </span>
        )}
      </span>
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-0.5 h-3.5 w-3.5 shrink-0 cursor-pointer"
        style={{ accentColor: "var(--th-accent)" }}
      />
    </label>
  );
}

function Btn({
  children,
  onClick,
  title,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className="rounded border px-2 py-1 text-xs hover:bg-neutral-700/30"
      style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
    >
      {children}
    </button>
  );
}
