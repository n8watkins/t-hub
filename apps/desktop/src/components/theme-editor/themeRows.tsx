// Theme-token row primitives (extracted verbatim from ThemeEditor.tsx). These
// are the per-token controls the Theme tabs render: a color swatch+hex row, a
// chrome-token color/slider/toggle wired by key, plus the compact icon button
// the preset actions use and the UI-font option list. They stay separate from
// the app-wide settingRows.tsx primitives because they're bound to the theme
// store's ChromeTokens shape, not plain callbacks.
import React from "react";
import { Switch } from "../settingRows";
import type { ChromeTokens } from "../../store/theme";

export const FONT_OPTIONS: { label: string; value: string }[] = [
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

/** A color control wired to a chrome token by key. */
export function ColorRow({
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
export function ColorInputRow({
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
    <div className="flex items-center justify-between gap-2 text-sm" title={hint}>
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
        className="h-6 w-7 shrink-0 cursor-pointer rounded border-0 bg-transparent p-0"
        aria-label={`${label} color`}
        title={`Pick ${label} color`}
      />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        className="w-[96px] shrink-0 rounded border bg-transparent px-1.5 py-1 font-mono text-xs"
        style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        aria-label={`${label} hex`}
        title={`${label} hex value`}
      />
    </div>
  );
}

/** A numeric slider wired to a chrome token by key. */
export function SliderRow({
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
    <div className="flex items-center justify-between gap-3 text-sm" title={hint}>
      <span className="shrink-0" style={{ color: "var(--th-fg)" }} title={hint}>
        {label}
      </span>
      <div className="flex items-center gap-3">
        <input
          type="range"
          min={min}
          max={max}
          step={1}
          value={value}
          onChange={(e) => set(k, Number(e.target.value) as ChromeTokens[typeof k])}
          className="w-36 cursor-pointer"
          style={{ accentColor: "var(--th-accent)" }}
          title={hint}
        />
        <span
          className="w-12 text-right font-mono text-xs"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {value}
          {suffix}
        </span>
      </div>
    </div>
  );
}

/**
 * A boolean toggle wired to a chrome token by key.
 *
 * The row is a plain <div> (NOT a <label>) and the label is plain text, so
 * clicking the name does nothing — only the Switch control itself toggles the
 * value. This avoids the surprise of flipping a setting by clicking its label.
 */
export function ToggleRow({
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
    <div
      className="flex items-center justify-between gap-3 text-sm"
      title={hint}
    >
      <span style={{ color: "var(--th-fg)" }}>{label}</span>
      <Switch
        checked={value}
        onChange={(v) => set(k, v as ChromeTokens[typeof k])}
        label={label}
      />
    </div>
  );
}

/** A compact, square icon button (themed) used for the preset actions. */
export function IconBtn({
  children,
  onClick,
  title,
  label,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title?: string;
  /** Accessible name (the icon carries no text). */
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-label={label}
      className="flex h-7 w-7 shrink-0 items-center justify-center rounded border transition-colors hover:bg-neutral-700/30"
      style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
    >
      {children}
    </button>
  );
}
