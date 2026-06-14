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
// sections — **General** (app behavior flags from useSettings) and **Theme**.
// Theme carries a lot of controls, so it has its own second-level sub-nav: the
// Preset group is pinned at the top (preset switch, Save-as-preset, Import/
// Export JSON — themes are shareable text, like VS Code), and the rest is split
// into tabs (Colors / Layout / Typography / Terminal) so the user focuses on
// one panel at a time instead of one long scroll. Each control is bound
// straight to a store's setters, so editing is live.
import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  useTheme,
  BUILTIN_PRESETS,
  type ChromeTokens,
  type AnsiPalette,
} from "../store/theme";
import {
  useSettings,
  TITLEBAR_HIDE_DELAY_MIN,
  TITLEBAR_HIDE_DELAY_MAX,
  TITLEBAR_REVEAL_ANIM_MIN,
  TITLEBAR_REVEAL_ANIM_MAX,
} from "../store/settings";
// Build-time app version/name from package.json (resolveJsonModule is on). Used
// as the fallback / "build" version; the live Tauri runtime version is fetched
// at runtime via getVersion() in the About group.
import pkg from "../../package.json";

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
        // Fixed height (capped at 85vh on short viewports) so switching between
        // the General and Theme sections never resizes the frame - only the
        // scrollable body inside changes. The panel stays the same size always.
        // Sized generously (within the 92vw / 85vh caps) so the many controls
        // and now-larger labels have room to breathe and read comfortably.
        className="flex h-[760px] max-h-[85vh] w-[900px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
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
          className="flex shrink-0 items-center justify-between border-b px-5 py-3.5"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div className="text-base font-semibold">Settings</div>
          <button
            type="button"
            onClick={onClose}
            className="-mr-1 flex h-8 w-8 items-center justify-center rounded transition-colors hover:bg-neutral-700/40"
            title="Close (Esc · Ctrl/Cmd+,)"
            aria-label="Close settings"
            style={{ color: "var(--th-fg-muted)" }}
          >
            <CloseIcon />
          </button>
        </div>

        {/* Body: left nav (top-level sections) + scrollable content pane. */}
        <div className="flex min-h-0 flex-1">
          <SectionNav active={section} onSelect={setSection} />
          <div className="th-scroll min-h-0 flex-1 overflow-y-auto px-5 py-4">
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
      className="flex w-44 shrink-0 flex-col gap-0.5 border-r p-2.5"
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
            className="rounded px-2.5 py-2 text-left text-sm transition-colors hover:bg-neutral-700/30"
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
  const titlebarHideDelayMs = useSettings((s) => s.titlebarHideDelayMs);
  const setTitlebarHideDelayMs = useSettings((s) => s.setTitlebarHideDelayMs);
  const titlebarRevealAnimMs = useSettings((s) => s.titlebarRevealAnimMs);
  const setTitlebarRevealAnimMs = useSettings((s) => s.setTitlebarRevealAnimMs);

  return (
    <>
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
        <SettingSliderRow
          label="Auto-hide delay"
          hint="How long the auto-hidden titlebar stays visible after a maximize or after the pointer leaves it, before it hides again."
          value={titlebarHideDelayMs}
          min={TITLEBAR_HIDE_DELAY_MIN}
          max={TITLEBAR_HIDE_DELAY_MAX}
          step={100}
          suffix="ms"
          onChange={setTitlebarHideDelayMs}
        />
        <SettingSliderRow
          label="Reveal animation"
          hint="Duration of the titlebar show/hide slide animation."
          value={titlebarRevealAnimMs}
          min={TITLEBAR_REVEAL_ANIM_MIN}
          max={TITLEBAR_REVEAL_ANIM_MAX}
          step={10}
          suffix="ms"
          onChange={setTitlebarRevealAnimMs}
        />
      </Group>

      <AboutGroup />
    </>
  );
}

/**
 * About — shows the app name and version so the user can tell what build they're
 * running. The real runtime version comes from Tauri's getVersion() (the actual
 * packaged app version); we fall back to the build-time package.json version
 * when not running inside Tauri (e.g. a plain `pnpm dev` browser session).
 */
function AboutGroup() {
  const [runtimeVersion, setRuntimeVersion] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    getVersion()
      .then((v) => {
        if (!disposed) setRuntimeVersion(v);
      })
      .catch(() => {
        // Not inside Tauri (or the call failed) — keep the package.json fallback.
      });
    return () => {
      disposed = true;
    };
  }, []);

  const version = runtimeVersion ?? pkg.version;
  return (
    <Group title="About" description="Which build of TermHub you're running.">
      <Row label="App">
        <span style={{ color: "var(--th-fg)" }}>T-Hub</span>
      </Row>
      <Row label="Version">
        <span className="font-mono text-xs" style={{ color: "var(--th-fg)" }}>
          {version}
        </span>
      </Row>
      {/* When the runtime version differs from the bundled package.json (e.g. a
          dev build), surface the build-time version too for clarity. */}
      {runtimeVersion && runtimeVersion !== pkg.version && (
        <Row label="Build">
          <span
            className="font-mono text-xs"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {pkg.version}
          </span>
        </Row>
      )}
    </Group>
  );
}

// ---------------------------------------------------------------------------
// Theme section — presets, colors, status dots, layout, type, terminal palette.
//
// The Theme surface carries a lot of controls, so it is split into a second-
// level sub-nav: Presets stay pinned at the top (always reachable), and the
// rest of the controls are grouped into tabs (Colors / Layout / Typography /
// Terminal) so the user works through one focused panel at a time instead of
// one long scroll. Every control stays bound straight to `useTheme` setters.
// ---------------------------------------------------------------------------
type ThemeTabId = "colors" | "layout" | "typography" | "terminal";

const THEME_TABS: { id: ThemeTabId; label: string }[] = [
  { id: "colors", label: "Colors" },
  { id: "layout", label: "Layout" },
  { id: "typography", label: "Typography" },
  { id: "terminal", label: "Terminal" },
];

function ThemeSection() {
  const [tab, setTab] = useState<ThemeTabId>("colors");
  return (
    <div className="flex flex-col gap-4">
      {/* Presets stay pinned at the top so switching/saving is always one click
          away regardless of which sub-tab is open. */}
      <PresetGroup />

      {/* Second-level segmented nav: pick one focused panel at a time. */}
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

/** The Theme sub-navigation: a themed segmented control (tabs). */
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

/** Presets / share — pinned at the top of the Theme section. */
function PresetGroup() {
  const active = useTheme((s) => s.active);
  const presets = useTheme((s) => s.presets);
  const applyPreset = useTheme((s) => s.applyPreset);
  const saveAsPreset = useTheme((s) => s.saveAsPreset);
  const deletePreset = useTheme((s) => s.deletePreset);
  const exportJSON = useTheme((s) => s.exportJSON);
  const importJSON = useTheme((s) => s.importJSON);
  const resetToDefault = useTheme((s) => s.resetToDefault);

  const presetNames = [
    ...BUILTIN_PRESETS.map((p) => p.name),
    ...Object.keys(presets),
  ];
  const isUserPreset = (name: string) =>
    Object.prototype.hasOwnProperty.call(presets, name);

  return (
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
      <div className="flex items-center gap-1.5">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Preset name"
          className="min-w-0 flex-1 rounded border bg-transparent px-2 py-1.5 text-sm"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        />
        {/* Compact icon actions (same behavior as the old text buttons). */}
        <IconBtn onClick={() => onSave(name)} title="Save the current theme as a named preset" label="Save preset">
          <SaveIcon />
        </IconBtn>
        <IconBtn onClick={doExport} title="Export: copy the active theme as JSON to the clipboard" label="Export theme">
          <ExportIcon />
        </IconBtn>
        <IconBtn onClick={() => fileRef.current?.click()} title="Import a theme from a JSON file" label="Import theme">
          <ImportIcon />
        </IconBtn>
        <IconBtn onClick={onReset} title="Reset to the Midnight default" label="Reset theme">
          <ResetIcon />
        </IconBtn>
        {canDelete && (
          <Btn onClick={onDelete} title="Delete this user preset">
            Delete
          </Btn>
        )}
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
        <div className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
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
        className="text-xs font-semibold uppercase tracking-wide"
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

function Row({ label, children }: { label: string; children: React.ReactNode }) {
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
      className="w-full cursor-pointer rounded border px-2 py-1.5 text-sm outline-none"
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

/**
 * A boolean toggle wired to a plain callback (used for settings-store flags,
 * which aren't chrome tokens). Same visual style as {@link ToggleRow}, with an
 * optional muted helper line under the label.
 *
 * Like {@link ToggleRow}, the row is a plain <div> and the label is plain text:
 * only the Switch toggles the value, never the label/helper text.
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
    <div className="flex items-start justify-between gap-3 text-sm">
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
        <Switch checked={value} onChange={onChange} label={label} />
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
function SettingSliderRow({
  label,
  value,
  min,
  max,
  step = 1,
  suffix,
  onChange,
  hint,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  suffix?: string;
  onChange: (v: number) => void;
  hint?: string;
}) {
  return (
    <div className="flex items-start justify-between gap-3 text-sm">
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
          onChange={(e) => onChange(Number(e.target.value))}
          className="w-36 cursor-pointer"
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
      className="rounded border px-2.5 py-1.5 text-sm hover:bg-neutral-700/30"
      style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
    >
      {children}
    </button>
  );
}

/** A compact, square icon button (themed) used for the preset actions. */
function IconBtn({
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

/**
 * A small switch-style toggle drawn from theme vars (no external CSS). Behaves
 * exactly like a checkbox: a hidden native checkbox carries focus/accessibility
 * while the pill + knob render the visual state. The track tints to the accent
 * when on.
 */
function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
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
        onChange={(e) => onChange(e.target.checked)}
        aria-label={label}
        className="absolute inset-0 m-0 cursor-pointer opacity-0"
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

// ---------------------------------------------------------------------------
// Inline SVG icons (stroke uses currentColor so they inherit the themed fg).
// ---------------------------------------------------------------------------
function Svg({ children }: { children: React.ReactNode }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

/** Larger X for the modal close button. */
function CloseIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M18 6 6 18" />
      <path d="m6 6 12 12" />
    </svg>
  );
}

/** Floppy-disk / save. */
function SaveIcon() {
  return (
    <Svg>
      <path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2Z" />
      <path d="M17 21v-8H7v8" />
      <path d="M7 3v5h8" />
    </Svg>
  );
}

/** Up-arrow into a tray - export. */
function ExportIcon() {
  return (
    <Svg>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M12 3v12" />
      <path d="m7 8 5-5 5 5" />
    </Svg>
  );
}

/** Down-arrow into a tray - import. */
function ImportIcon() {
  return (
    <Svg>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M12 3v12" />
      <path d="m7 10 5 5 5-5" />
    </Svg>
  );
}

/** Circular arrow - reset. */
function ResetIcon() {
  return (
    <Svg>
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
    </Svg>
  );
}

/** A small disclosure chevron; rotates when open. */
function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className="shrink-0 transition-transform"
      style={{
        color: "var(--th-fg-muted)",
        transform: open ? "rotate(90deg)" : "rotate(0deg)",
      }}
    >
      <path d="m9 18 6-6-6-6" />
    </svg>
  );
}
