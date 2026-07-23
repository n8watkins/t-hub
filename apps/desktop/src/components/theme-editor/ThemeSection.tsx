// Theme section — presets, colors, status dots, layout, type, terminal palette
// (extracted verbatim from ThemeEditor.tsx).
//
// The Theme surface carries a lot of controls, so it is ONE left-nav page whose
// sub-panels (Preset / Colors / Layout / Typography / Terminal) are horizontal
// peer tabs on top (NOT separate left-nav items). Preset is pinned above the
// tabs so switching/saving is always reachable, and the rest of the controls
// are grouped into tabs so the user works through one focused panel at a time
// instead of one long scroll. Every control stays bound straight to `useTheme`.
import { useState } from "react";
import { Btn, Group, Opt, Row, ThemeSelect } from "../settingRows";
import { useTheme, type AnsiPalette } from "../../store/theme";
import { PresetGroup } from "./PresetGroup";
import {
  ColorRow,
  ColorInputRow,
  SliderRow,
  ToggleRow,
  FONT_OPTIONS,
} from "./themeRows";
import { Chevron } from "./themeIcons";

type ThemeTabId = "colors" | "layout" | "typography" | "terminal";

const THEME_TABS: { id: ThemeTabId; label: string }[] = [
  { id: "colors", label: "Colors" },
  { id: "layout", label: "Layout" },
  { id: "typography", label: "Typography" },
  { id: "terminal", label: "Terminal" },
];

export function ThemeSection() {
  const [tab, setTab] = useState<ThemeTabId>("colors");
  return (
    <div className="flex flex-col gap-4">
      {/* Presets pinned at the top so switching/saving is always one click away. */}
      <PresetGroup />
      {/* Horizontal sub-tabs: one focused panel at a time. */}
      <ThemeTabs active={tab} onSelect={setTab} />
      <div>
        {tab === "colors" && <ColorsTab />}
        {tab === "layout" && <LayoutTab />}
        {tab === "typography" && <TypographyTab />}
        {tab === "terminal" && <TerminalGroup />}
      </div>
    </div>
  );
}

/** The Theme page's horizontal sub-navigation (a themed segmented control). */
function ThemeTabs({
  active,
  onSelect,
}: {
  active: ThemeTabId;
  onSelect: (t: ThemeTabId) => void;
}) {
  return (
    <div
      className="flex gap-1 rounded-md border p-1"
      role="tablist"
      aria-label="Theme sections"
      style={{ borderColor: "var(--th-border)" }}
    >
      {THEME_TABS.map((t) => {
        const isActive = t.id === active;
        return (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={isActive}
            onClick={() => onSelect(t.id)}
            className="flex-1 rounded px-3 py-1.5 text-center text-sm transition-colors hover:bg-neutral-700/30"
            style={{
              backgroundColor: isActive ? "var(--th-tile-bg)" : "transparent",
              color: isActive ? "var(--th-fg)" : "var(--th-fg-muted)",
              fontWeight: isActive ? 600 : 400,
            }}
          >
            {t.label}
          </button>
        );
      })}
    </div>
  );
}

/** Colors tab — chrome colors + the per-state status dots. */
function ColorsTab() {
  const active = useTheme((s) => s.active);
  const setChromeToken = useTheme((s) => s.setChromeToken);
  const c = active.chrome;
  return (
    <>
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
    </>
  );
}

/** Layout tab — sizing sliders + header visibility toggles. */
function LayoutTab() {
  const active = useTheme((s) => s.active);
  const setChromeToken = useTheme((s) => s.setChromeToken);
  const c = active.chrome;
  return (
    <Group title="Layout">
      <SliderRow
        label="Tile header height"
        hint="Height of the header bar at the top of each tile (px). Dense two-row headers reserve at least 52px."
        k="tileHeaderHeight"
        value={c.tileHeaderHeight}
        min={30}
        max={80}
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
  );
}

/** Typography tab — UI font family + base font size. */
function TypographyTab() {
  const active = useTheme((s) => s.active);
  const setChromeToken = useTheme((s) => s.setChromeToken);
  const c = active.chrome;
  return (
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
  );
}

// ---------------------------------------------------------------------------
// Terminal palette group (optional palette; xterm ITheme is applied elsewhere).
// ---------------------------------------------------------------------------
function TerminalGroup() {
  const active = useTheme((s) => s.active);
  const setTerminalToken = useTheme((s) => s.setTerminalToken);
  const setAnsiColor = useTheme((s) => s.setAnsiColor);
  const resetTerminalPalette = useTheme((s) => s.resetTerminalPalette);
  const term = active.terminal;
  // Collapsed by default so the advanced ANSI slots stay tucked away and the
  // section doesn't read as front-and-center.
  const [showAnsi, setShowAnsi] = useState(false);
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

      {/* Advanced ANSI palette: tucked behind a collapsible row so it isn't
          front-and-center. The 16 slots are fixed ANSI roles; only the colors
          are editable, and a Reset restores the theme default. */}
      <div
        className="mt-3 rounded border"
        style={{ borderColor: "var(--th-border)" }}
      >
        <button
          type="button"
          onClick={() => setShowAnsi((v) => !v)}
          className="flex w-full items-center gap-1.5 px-2.5 py-2 text-left"
          aria-expanded={showAnsi}
          title="Advanced: the 16 base ANSI terminal colors"
        >
          <Chevron open={showAnsi} />
          <span
            className="text-xs font-semibold uppercase tracking-wide"
            style={{ color: "var(--th-fg)" }}
          >
            ANSI palette
          </span>
          <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
            (advanced)
          </span>
        </button>
        {showAnsi && (
          <div className="px-2.5 pb-2.5">
            <p
              className="text-xs leading-snug"
              style={{ color: "var(--th-fg-muted)" }}
            >
              These are the 16 base colors terminal programs use to draw text.
              The slot names (black, red, ...) are fixed ANSI roles - only
              their colors are editable. Use Reset palette to restore the
              defaults.
            </p>
            <div className="mt-1.5 flex justify-end">
              <Btn
                onClick={resetTerminalPalette}
                title="Restore the default terminal background, foreground, cursor, selection, and all 16 ANSI colors"
              >
                Reset palette
              </Btn>
            </div>
            <div className="mt-1.5 grid grid-cols-2 gap-x-3">
              {ansiKeys.map((k) => (
                <ColorInputRow
                  key={k}
                  label={k}
                  value={term.ansi[k]}
                  onChange={(v) => setAnsiColor(k, v)}
                  labelTitle={`ANSI "${k}" slot - a fixed role name; only its color is editable`}
                />
              ))}
            </div>
          </div>
        )}
      </div>
    </Group>
  );
}
