// ThemeEditor — the live "settings" surface the app was missing (PRD §5.5).
//
// The whole user-facing promise of the theming system lives here: a person
// customizes TermHub's look WITHOUT editing config files, and every change is
// instant (each control writes a token into the theme store, which writes a CSS
// var, which re-renders the chrome). It is a fully self-contained overlay:
//   - It owns its own open/closed state and a global `Ctrl/Cmd+,` keydown
//     listener to toggle itself, so App.tsx never has to know it exists.
//   - It renders nothing until opened (a fixed, right-anchored panel + scrim).
//   - Esc closes it; a click on the scrim closes it.
//
// Controls are grouped (Presets, Colors, Layout, Typography, Terminal) and each
// is bound straight to the store's per-token setters, so editing is live. The
// Presets group also does Save-as-preset, preset switching, and Import/Export
// JSON (themes are shareable text, like VS Code).
import { useEffect, useRef, useState } from "react";
import {
  useTheme,
  BUILTIN_PRESETS,
  type ChromeTokens,
  type AnsiPalette,
} from "../store/theme";

/** Toggle the editor with Ctrl/Cmd+, (and let Esc close it). Self-contained. */
function useEditorToggle(): [boolean, (open: boolean) => void] {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      // Ctrl/Cmd+, opens/closes the editor (matches the conventional "settings"
      // shortcut). `e.key` is "," regardless of layout shifts for this combo.
      if (mod && e.key === "," && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        setOpen((v) => !v);
      } else if (e.key === "Escape") {
        // Only consume Escape when we're actually open (don't swallow it
        // globally — terminals/inputs may want it when the panel is closed).
        setOpen((v) => {
          if (v) {
            e.preventDefault();
            return false;
          }
          return v;
        });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return [open, setOpen];
}

export function ThemeEditor() {
  const [open, setOpen] = useEditorToggle();
  if (!open) return null;
  return <ThemeEditorPanel onClose={() => setOpen(false)} />;
}

// ---------------------------------------------------------------------------
// The panel (only mounted while open).
// ---------------------------------------------------------------------------
function ThemeEditorPanel({ onClose }: { onClose: () => void }) {
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
    // Scrim: a click anywhere outside the panel closes the editor. The panel
    // itself stops propagation so inner clicks don't bubble to the scrim.
    <div
      className="fixed inset-0 z-50 flex justify-end"
      onMouseDown={onClose}
      // The bootstrap host is pointer-events:none (so it's inert when closed);
      // re-enable events on the open overlay so the scrim + panel are clickable.
      style={{ backgroundColor: "rgba(0,0,0,0.35)", pointerEvents: "auto" }}
    >
      <div
        className="flex h-full w-[360px] flex-col border-l shadow-2xl"
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
          <div className="text-sm font-semibold">Theme</div>
          <button
            type="button"
            onClick={onClose}
            className="rounded px-1.5 leading-none hover:bg-neutral-700/40"
            title="Close (Esc · Ctrl/Cmd+,)"
            aria-label="Close theme editor"
            style={{ color: "var(--th-fg-muted)" }}
          >
            ×
          </button>
        </div>

        {/* Scrollable body of grouped controls */}
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {/* --- Presets / share --- */}
          <Group title="Preset">
            <Row label="Active">
              <select
                value={presetNames.includes(active.name) ? active.name : ""}
                onChange={(e) => applyPreset(e.target.value)}
                className="w-full rounded border bg-transparent px-2 py-1 text-xs"
                style={{ borderColor: "var(--th-border)" }}
              >
                {!presetNames.includes(active.name) && (
                  <option value="">{active.name} (edited)</option>
                )}
                {presetNames.map((n) => (
                  <option key={n} value={n} style={{ color: "#000" }}>
                    {n}
                    {isUserPreset(n) ? " ·" : ""}
                  </option>
                ))}
              </select>
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
          <Group title="Colors">
            <ColorRow label="Accent" k="accent" value={c.accent} set={setChromeToken} />
            <ColorRow label="Focus ring" k="focusRing" value={c.focusRing} set={setChromeToken} />
            <ColorRow label="App background" k="appBg" value={c.appBg} set={setChromeToken} />
            <ColorRow label="Tile background" k="tileBg" value={c.tileBg} set={setChromeToken} />
            <ColorRow label="Header background" k="headerBg" value={c.headerBg} set={setChromeToken} />
            <ColorRow label="Sidebar background" k="sidebarBg" value={c.sidebarBg} set={setChromeToken} />
            <ColorRow label="Titlebar background" k="titlebarBg" value={c.titlebarBg} set={setChromeToken} />
            <ColorRow label="Border" k="border" value={c.border} set={setChromeToken} />
            <ColorRow label="Text" k="fgPrimary" value={c.fgPrimary} set={setChromeToken} />
            <ColorRow label="Muted text" k="fgMuted" value={c.fgMuted} set={setChromeToken} />
          </Group>

          {/* --- Status dots --- */}
          <Group title="Status dots">
            <ColorRow label="Starting" k="dotStarting" value={c.dotStarting} set={setChromeToken} />
            <ColorRow label="Live" k="dotLive" value={c.dotLive} set={setChromeToken} />
            <ColorRow label="Detached" k="dotDetached" value={c.dotDetached} set={setChromeToken} />
            <ColorRow label="Exited" k="dotExited" value={c.dotExited} set={setChromeToken} />
            <ColorRow label="Error" k="dotError" value={c.dotError} set={setChromeToken} />
          </Group>

          {/* --- Layout --- */}
          <Group title="Layout">
            <SliderRow
              label="Tile header height"
              k="tileHeaderHeight"
              value={c.tileHeaderHeight}
              min={16}
              max={40}
              suffix="px"
              set={setChromeToken}
            />
            <SliderRow
              label="Focus ring width"
              k="focusRingWidth"
              value={c.focusRingWidth}
              min={0}
              max={4}
              suffix="px"
              set={setChromeToken}
            />
            <SliderRow
              label="Grid gap"
              k="gridGap"
              value={c.gridGap}
              min={0}
              max={24}
              suffix="px"
              set={setChromeToken}
            />
            <SliderRow
              label="Corner radius"
              k="cornerRadius"
              value={c.cornerRadius}
              min={0}
              max={20}
              suffix="px"
              set={setChromeToken}
            />
            <ToggleRow
              label="Show tile header"
              k="showTileHeader"
              value={c.showTileHeader}
              set={setChromeToken}
            />
            <ToggleRow
              label="Header on hover only"
              k="headerOnHover"
              value={c.headerOnHover}
              set={setChromeToken}
            />
            <ToggleRow label="Show cwd" k="showCwd" value={c.showCwd} set={setChromeToken} />
          </Group>

          {/* --- Typography --- */}
          <Group title="Typography">
            <Row label="UI font">
              <select
                value={c.fontFamily}
                onChange={(e) => setChromeToken("fontFamily", e.target.value)}
                className="w-full rounded border bg-transparent px-2 py-1 text-xs"
                style={{ borderColor: "var(--th-border)" }}
              >
                {FONT_OPTIONS.map((f) => (
                  <option key={f.label} value={f.value} style={{ color: "#000" }}>
                    {f.label}
                  </option>
                ))}
              </select>
            </Row>
            <SliderRow
              label="Base font size"
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
        </div>
      </div>
    </div>
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
        <Btn onClick={() => onSave(name)}>Save</Btn>
        {canDelete && (
          <Btn onClick={onDelete} title="Delete this user preset">
            Delete
          </Btn>
        )}
      </div>
      <div className="flex gap-1.5">
        <Btn onClick={doExport}>Export</Btn>
        <Btn onClick={() => fileRef.current?.click()}>Import</Btn>
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
    <Group title="Terminal palette">
      <ColorInputRow
        label="Background"
        value={term.background}
        onChange={(v) => setTerminalToken({ background: v })}
      />
      <ColorInputRow
        label="Foreground"
        value={term.foreground}
        onChange={(v) => setTerminalToken({ foreground: v })}
      />
      <ColorInputRow
        label="Cursor"
        value={term.cursor}
        onChange={(v) => setTerminalToken({ cursor: v })}
      />
      <ColorInputRow
        label="Selection"
        value={term.selection}
        onChange={(v) => setTerminalToken({ selection: v })}
      />
      <div className="mt-1.5 grid grid-cols-2 gap-x-3">
        {ansiKeys.map((k) => (
          <ColorInputRow
            key={k}
            label={k}
            value={term.ansi[k]}
            onChange={(v) => setAnsiColor(k, v)}
          />
        ))}
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

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-4">
      <div
        className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {title}
      </div>
      <div className="flex flex-col gap-1.5">{children}</div>
    </section>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex items-center justify-between gap-3 text-xs">
      <span className="shrink-0" style={{ color: "var(--th-fg)" }}>
        {label}
      </span>
      <span className="flex min-w-0 flex-1 justify-end">{children}</span>
    </label>
  );
}

/** A color control wired to a chrome token by key. */
function ColorRow({
  label,
  k,
  value,
  set,
}: {
  label: string;
  k: keyof ChromeTokens;
  value: string;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
}) {
  return (
    <ColorInputRow
      label={label}
      value={value}
      onChange={(v) => set(k, v as ChromeTokens[typeof k])}
    />
  );
}

/**
 * A raw color control: a native swatch + a hex text input. Some tokens carry an
 * 8-digit (alpha) hex; the native picker can't show alpha, so we keep the text
 * field as the source of truth and only feed the picker the leading #rrggbb.
 */
function ColorInputRow({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
}) {
  const swatch = value.startsWith("#") ? value.slice(0, 7) : "#000000";
  return (
    <label className="flex items-center justify-between gap-2 text-xs">
      <span className="min-w-0 flex-1 truncate" style={{ color: "var(--th-fg)" }}>
        {label}
      </span>
      <input
        type="color"
        value={swatch}
        onChange={(e) => onChange(e.target.value)}
        className="h-5 w-6 shrink-0 cursor-pointer rounded border-0 bg-transparent p-0"
        aria-label={`${label} color`}
      />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        className="w-[88px] shrink-0 rounded border bg-transparent px-1.5 py-0.5 font-mono text-[11px]"
        style={{ borderColor: "var(--th-border)" }}
        aria-label={`${label} hex`}
      />
    </label>
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
}: {
  label: string;
  k: keyof ChromeTokens;
  value: number;
  min: number;
  max: number;
  suffix?: string;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-xs">
      <span className="shrink-0" style={{ color: "var(--th-fg)" }}>
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
}: {
  label: string;
  k: keyof ChromeTokens;
  value: boolean;
  set: <K extends keyof ChromeTokens>(key: K, v: ChromeTokens[K]) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between gap-3 text-xs">
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
