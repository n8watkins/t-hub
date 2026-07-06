// Shared Settings-surface primitives, extracted verbatim from ThemeEditor.tsx
// so section components can live in their own files (and their tests can
// render them without importing the whole editor). ThemeEditor re-imports
// these; VoiceSettings is the first external consumer.
//
// `disabled` was added to ThemeSelect and SettingSliderRow during the
// extraction (SettingToggleRow and Btn already had it) - the Voice section's
// degradation state dims every control except its master toggle.
import React from "react";

export function Group({
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
    // A divider line + top padding between groups so subsections read as
    // distinct (the first group in a pane drops the rule). Header bumped up a
    // step so sub-section titles are easier to scan.
    <section
      className="mb-5 border-t pt-4 first:border-t-0 first:pt-0"
      style={{ borderColor: "var(--th-border)" }}
    >
      <div
        className="text-[13px] font-semibold uppercase tracking-wide"
        style={{ color: "var(--th-fg)" }}
      >
        {title}
      </div>
      {description && (
        <p
          className="mb-2 mt-1 text-xs leading-snug"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {description}
        </p>
      )}
      <div
        className={
          (description ? "" : "mt-2 ") +
          (cols === 2
            ? "grid grid-cols-2 gap-x-5 gap-y-2"
            : "flex flex-col gap-2")
        }
      >
        {children}
      </div>
    </section>
  );
}

export function Row({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-sm">
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
export function ThemeSelect({
  value,
  onChange,
  title,
  children,
  disabled = false,
}: {
  value: string;
  onChange: (v: string) => void;
  title?: string;
  children: React.ReactNode;
  disabled?: boolean;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      title={title}
      disabled={disabled}
      className="w-full cursor-pointer rounded border px-2 py-1.5 text-sm outline-none disabled:cursor-not-allowed disabled:opacity-50"
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
export function Opt({
  value,
  children,
}: {
  value: string;
  children: React.ReactNode;
}) {
  return (
    <option
      value={value}
      style={{ backgroundColor: "var(--th-tile-bg)", color: "var(--th-fg)" }}
    >
      {children}
    </option>
  );
}

export function SettingToggleRow({
  label,
  value,
  onChange,
  hint,
  disabled = false,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
  hint?: string;
  /** Dim the row + make the switch inert (e.g. a dependent setting whose parent
   *  toggle is off). */
  disabled?: boolean;
}) {
  return (
    <div
      className="flex items-start justify-between gap-3 text-sm"
      style={disabled ? { opacity: 0.5 } : undefined}
    >
      <span className="flex min-w-0 flex-col">
        <span style={{ color: "var(--th-fg)" }}>{label}</span>
        {hint && (
          <span
            className="mt-1 text-xs leading-snug"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {hint}
          </span>
        )}
      </span>
      <span className="mt-0.5 shrink-0">
        <Switch
          checked={value}
          onChange={onChange}
          label={label}
          disabled={disabled}
        />
      </span>
    </div>
  );
}

/**
 * A numeric slider wired to a plain callback (used for settings-store values,
 * which aren't chrome tokens). Same label + helper-text layout as
 * {@link SettingToggleRow}; only the slider changes the value (the label and
 * helper text are inert), and the current value is shown beside the track.
 */
export function SettingSliderRow({
  label,
  value,
  min,
  max,
  step = 1,
  suffix,
  onChange,
  hint,
  disabled = false,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  suffix?: string;
  onChange: (v: number) => void;
  hint?: string;
  disabled?: boolean;
}) {
  return (
    <div
      className="flex items-start justify-between gap-3 text-sm"
      style={disabled ? { opacity: 0.5 } : undefined}
    >
      <span className="flex min-w-0 flex-col">
        <span style={{ color: "var(--th-fg)" }}>{label}</span>
        {hint && (
          <span
            className="mt-1 text-xs leading-snug"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {hint}
          </span>
        )}
      </span>
      <span className="mt-0.5 flex shrink-0 items-center gap-3">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          disabled={disabled}
          onChange={(e) => onChange(Number(e.target.value))}
          className="w-36 cursor-pointer disabled:cursor-not-allowed"
          style={{ accentColor: "var(--th-accent)" }}
          aria-label={label}
        />
        <span
          className="w-16 text-right font-mono text-xs"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {value}
          {suffix}
        </span>
      </span>
    </div>
  );
}

export function Btn({
  children,
  onClick,
  title,
  disabled = false,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      disabled={disabled}
      className="rounded border px-2.5 py-1.5 text-sm hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-50"
      style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
    >
      {children}
    </button>
  );
}

/**
 * A small switch-style toggle drawn from theme vars (no external CSS). Behaves
 * exactly like a checkbox: a hidden native checkbox carries focus/accessibility
 * while the pill + knob render the visual state. The track tints to the accent
 * when on.
 */
export function Switch({
  checked,
  onChange,
  label,
  disabled = false,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <span
      className="relative inline-flex h-[18px] w-[32px] shrink-0 items-center rounded-full transition-colors"
      style={{
        backgroundColor: checked ? "var(--th-accent)" : "var(--th-border)",
      }}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        aria-label={label}
        className={`absolute inset-0 m-0 opacity-0 ${
          disabled ? "cursor-not-allowed" : "cursor-pointer"
        }`}
      />
      <span
        className="pointer-events-none absolute h-[13px] w-[13px] rounded-full transition-transform"
        style={{
          backgroundColor: "var(--th-fg)",
          left: 2,
          transform: checked ? "translateX(14px)" : "translateX(0)",
        }}
      />
    </span>
  );
}
