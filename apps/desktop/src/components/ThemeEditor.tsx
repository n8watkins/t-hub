// ThemeEditor — the live Settings surface the app was missing (PRD §5.5).
//
// The whole user-facing promise of the theming system lives here: a person
// customizes T-Hub's look WITHOUT editing config files, and every change is
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
// Shared settings-row primitives (extracted so section components can live in
// their own files - see settingRows.tsx; VoiceSection uses them too).
import {
  Btn,
  Group,
  Opt,
  Row,
  SettingSliderRow,
  SettingToggleRow,
  Switch,
  ThemeSelect,
} from "./settingRows";
import { VoiceSection } from "./VoiceSettings";
// --- feat/auto-updater: in-app "Updates" section -------------------------------
// check() resolves the signed update package (same call the on-launch mount and
// the manual Install button use); relaunch() restarts the app after install;
// detectUpdate() is the tolerant availability probe over latest.json.
import { check as checkUpdaterPackage } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { detectUpdate, RELEASES_URL, type UpdateCheckResult } from "../lib/updates";
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
import { FILE_ICON_THEMES } from "../lib/fileIcons";
// Build-time app version/name from package.json (resolveJsonModule is on). Used
// as the fallback / "build" version; the live Tauri runtime version is fetched
// at runtime via getVersion() in the About group.
import pkg from "../../package.json";
// Recovery review (#recovery): a self-contained modal opened from a button in the
// General section. It renders its own scrim/panel above this one (z-[60] > z-50).
import { RecoveryReview } from "./RecoveryReview";
import { StatusIndicator, type StatusVariant } from "./StatusIndicator";
// Claude hooks install/uninstall now lives in Settings (moved out of the sidebar).
import { HookInstallPanel } from "./HookInstallPanel";
import { claudeHooksInstalled } from "../ipc/client05";
// Hybrid keymap (WS-3): the interactive Keyboard section shows every command's
// live direct/prefixed binding, opens the palette to rebind, and resets defaults.
import { useKeybindings } from "../store/keybindings";
import { COMMANDS } from "../lib/commands";
import { useAppName } from "../lib/appName";
import { formatChord } from "../lib/chord";
import { openKeyboardPalette } from "./CommandPalette";
// Rules (WS-5b): the event→action engine's config surface. The list + CRUD live
// in store/rules; lib/rulesMount does the firing.
import {
  useRules,
  TRIGGER_STATUSES,
  ACTION_KINDS,
  statusLabel,
  actionKindLabel,
  type Rule,
  type ActionKind,
} from "../store/rules";
import type { SessionStatus } from "../ipc/model";

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
// One flat nav of sections (the old two-level General/Theme + Theme sub-tabs are
// gone — everything is reachable straight from the left nav now).
type SectionId =
  | "general"
  | "voice"
  | "hotkeys"
  | "keyboard"
  | "rules"
  | "hooks"
  | "updates"
  | "about"
  | "setup"
  | "theme";

function ThemeEditorPanel({ onClose }: { onClose: () => void }) {
  // Honor a deep-link target (e.g. the sidebar "install hooks" hint opens us
  // straight to Hooks) captured when the panel was opened; default to General.
  const target = useSettings.getState().settingsSection as SectionId | null;
  const [section, setSection] = useState<SectionId>(target ?? "general");

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
            <SectionContent section={section} onNavigate={setSection} />
          </div>
        </div>
      </div>
    </div>
  );
}

/** The left-hand nav — a single flat list (grouped under small APP / THEME
 *  labels) so every section is one click away, with no second-level top tabs. */
function SectionNav({
  active,
  onSelect,
}: {
  active: SectionId;
  onSelect: (s: SectionId) => void;
}) {
  const groups: {
    label: string;
    items: { id: SectionId; label: string; hint?: string }[];
  }[] = [
    {
      label: "App",
      items: [
        { id: "general", label: "General", hint: "App behavior" },
        { id: "voice", label: "Voice", hint: "Spoken announcements via the local TTS server" },
        { id: "keyboard", label: "Keyboard", hint: "Rebindable command shortcuts + prefix" },
        { id: "hotkeys", label: "Hotkeys", hint: "Fixed app + terminal keys (the rebindable ones live in Keyboard)" },
        { id: "rules", label: "Rules", hint: "Run an action when a session changes status" },
        { id: "hooks", label: "Hooks", hint: "Claude Code lifecycle hooks" },
        { id: "updates", label: "Updates", hint: "Check for + install app updates" },
        { id: "about", label: "About", hint: "What T-Hub is + version" },
        { id: "setup", label: "Setup", hint: "How to use it" },
      ],
    },
    {
      label: "Theme",
      items: [
        { id: "theme", label: "Theme", hint: "Presets, colors, layout, type, terminal" },
      ],
    },
  ];
  return (
    <nav
      className="th-scroll flex w-44 shrink-0 flex-col gap-3 overflow-y-auto border-r p-2.5"
      style={{ borderColor: "var(--th-border)" }}
      aria-label="Settings sections"
    >
      {groups.map((g) => (
        <div key={g.label} className="flex flex-col gap-0.5">
          {/* Group label ("App" / "Theme") intentionally omitted — the section
              buttons below are self-explanatory and the headers just added
              clutter. The grouping (+ the gap between groups) is kept. */}
          {g.items.map((it) => {
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
        </div>
      ))}
    </nav>
  );
}

/** Render the panel for the selected nav section. `onNavigate` lets a section
 *  jump the panel to another section in place (e.g. General → Hooks). */
function SectionContent({
  section,
  onNavigate,
}: {
  section: SectionId;
  onNavigate: (s: SectionId) => void;
}) {
  switch (section) {
    case "general":
      return <GeneralSection onNavigate={onNavigate} />;
    case "voice":
      return <VoiceSection />;
    case "keyboard":
      return <KeyboardSection />;
    case "rules":
      return <RulesSection />;
    case "hotkeys":
      return <HotkeysSection />;
    case "hooks":
      return <HooksSection />;
    case "updates":
      return <UpdatesSection />;
    case "about":
      return <AboutSection />;
    case "setup":
      return <SetupSection />;
    case "theme":
      return <ThemeSection />;
  }
}

// ---------------------------------------------------------------------------
// General section — app behavior flags (settings store, not theme tokens).
// ---------------------------------------------------------------------------
function GeneralSection({ onNavigate }: { onNavigate: (s: SectionId) => void }) {
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
  const closeToTray = useSettings((s) => s.closeToTray);
  const setCloseToTray = useSettings((s) => s.setCloseToTray);
  const resumeStartsClaude = useSettings((s) => s.resumeStartsClaude);
  const setResumeStartsClaude = useSettings((s) => s.setResumeStartsClaude);
  const showHeaderContextMeter = useSettings((s) => s.showHeaderContextMeter);
  const setShowHeaderContextMeter = useSettings(
    (s) => s.setShowHeaderContextMeter,
  );
  const fileIconTheme = useSettings((s) => s.fileIconTheme);
  const setFileIconTheme = useSettings((s) => s.setFileIconTheme);
  // Recovery review modal open state (#recovery) — local to this section; the
  // modal is fully self-contained and renders its own overlay above the panel.
  const [recoveryOpen, setRecoveryOpen] = useState(false);

  return (
    <>
      <Group title="Sessions">
        <SettingToggleRow
          label="Resume starts Claude Code"
          hint="When you Resume a session from the sidebar's Recent list, run `claude --resume` to reopen that conversation. Turn off to just open a terminal in the session's directory instead."
          value={resumeStartsClaude}
          onChange={setResumeStartsClaude}
        />
      </Group>

      <Group
        title="Tiles"
        description="Chrome shown in each terminal tile's header."
      >
        <SettingToggleRow
          label="Show context meter in tile header"
          hint="Show a Claude session's context-window fullness (a thin bar + percentage) in the tile header. Off by default to keep the header uncluttered; the sidebar captain rows show context either way."
          value={showHeaderContextMeter}
          onChange={setShowHeaderContextMeter}
        />
      </Group>

      <Group
        title="Status indicators"
        description="The ring shown beside each terminal in the sidebar and on its tile header. For Claude it reflects the agent's reported status (needs the status hooks installed below); a plain shell only shows the running/empty states."
      >
        <StatusLegendRow
          variant="working"
          name="Working"
          hint="A spinner — the agent is thinking / actively generating a response (or a shell command is producing output)."
        />
        <StatusLegendRow
          variant="attention"
          name="Needs you"
          hint="A pulsing amber ring — the agent is asking a question or waiting on a permission prompt."
        />
        <StatusLegendRow
          variant="done"
          name="Done"
          hint="A solid green dot — the last turn finished and the session is ready for input."
        />
        <StatusLegendRow
          variant="idle"
          name="Idle"
          hint="A hollow green ring — an agent is open but not doing anything right now."
        />
        <StatusLegendRow
          variant="error"
          name="Error"
          hint="A solid red dot — the session failed."
        />
        <StatusLegendRow
          variant={null}
          name="Empty"
          hint="No indicator — a plain shell with nothing running."
        />
      </Group>

      <Group
        title="Files"
        description="How the Files panel draws file & folder icons."
      >
        <Row label="Icon theme">
          <ThemeSelect
            value={fileIconTheme}
            onChange={setFileIconTheme}
            title="Pick the icon set used in the Files tree"
          >
            {FILE_ICON_THEMES.map((t) => (
              <Opt key={t.id} value={t.id}>
                {t.label}
              </Opt>
            ))}
          </ThemeSelect>
        </Row>
      </Group>

      {/* The notification sound + desktop-notification toggles moved into
          Hooks → "Attention & notifications", next to the events that trigger
          them. Leave a pointer here so they're still discoverable from General. */}
      <Group
        title="Notifications"
        description="Notification sounds and desktop notifications now live with the attention hooks that fire them."
      >
        <Row label="Sounds & desktop alerts">
          <Btn
            onClick={() => onNavigate("hooks")}
            title="Open Hooks → Attention & notifications, where the sound and desktop-notification toggles live"
          >
            Open in Hooks →
          </Btn>
        </Row>
      </Group>

      <Group title="Window">
        <SettingToggleRow
          label="Close button hides to tray"
          hint="The titlebar × hides T-Hub to the system tray and keeps it running, so your sessions stay alive — reopen it from the tray icon. Turn off to make the × quit the app instead."
          value={closeToTray}
          onChange={setCloseToTray}
        />
      </Group>

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

      <Group
        title="Recovery"
        description="Roll the workspace back to a recent layout from the durable snapshot history (tabs, tile arrangement, focus). Live terminals are reconciled, never killed."
      >
        <Row label="Workspace layout">
          <Btn
            onClick={() => setRecoveryOpen(true)}
            title="Open the recovery review to preview and restore a recent workspace layout"
          >
            Recovery review…
          </Btn>
        </Row>
      </Group>

      {/* About moved to its own "About & Setup" nav section. */}

      {/* The recovery modal renders its own scrim/panel above this one. */}
      <RecoveryReview open={recoveryOpen} onClose={() => setRecoveryOpen(false)} />
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
  const appName = useAppName();
  return (
    <Group title="About" description="Which build of T-Hub you're running.">
      <Row label="App">
        <span style={{ color: "var(--th-fg)" }}>{appName}</span>
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
// ---------------------------------------------------------------------------
// Theme — ONE left-nav page whose sub-panels (Preset / Colors / Layout /
// Typography / Terminal) are horizontal peer tabs on top (NOT separate left-nav
// items). Preset is pinned above the tabs so switching/saving is always reachable.
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

// ---------------------------------------------------------------------------
// Keyboard (WS-3) — the interactive, REBINDABLE keymap. Shows the tmux-style
// prefix + every command's live direct/prefixed binding; "Rebind" opens the
// fuzzy command palette (where the press-new-key flow lives) and "Reset to
// defaults" restores the shipped bindings.
// ---------------------------------------------------------------------------
function KeyboardSection() {
  const prefixKey = useKeybindings((s) => s.prefixKey);
  const direct = useKeybindings((s) => s.direct);
  const prefixed = useKeybindings((s) => s.prefixed);
  const resetDefaults = useKeybindings((s) => s.resetDefaults);

  return (
    <>
      <Group
        title="Prefix"
        description="A tmux-style prefix arms an expanding command tail: press it, then a single key runs a prefixed command. Press it twice to send a literal prefix keystroke to the terminal."
      >
        <Row label="Prefix key">
          <kbd
            className="rounded border px-1.5 py-0.5 font-mono text-xs"
            style={{
              borderColor: "var(--th-border)",
              color: "var(--th-fg)",
              backgroundColor: "var(--th-tile-bg)",
            }}
          >
            {formatChord(prefixKey)}
          </kbd>
        </Row>
      </Group>

      <Group
        title="Commands"
        description="Each command can fire directly from a single chord and/or after the prefix. Open the command palette to change a direct shortcut interactively."
      >
        <div className="flex items-center justify-between gap-3">
          <button
            type="button"
            onClick={openKeyboardPalette}
            className="rounded border px-2.5 py-1 text-xs transition-colors hover:bg-neutral-700/30"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
          >
            Open command palette to rebind
          </button>
          <button
            type="button"
            onClick={resetDefaults}
            className="rounded border px-2.5 py-1 text-xs transition-colors hover:bg-neutral-700/30"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            title="Restore every shortcut + the prefix to the shipped defaults"
          >
            Reset to defaults
          </button>
        </div>
        {COMMANDS.map((cmd) => (
          <div
            key={cmd.id}
            className="flex items-center justify-between gap-3 text-sm"
          >
            <span style={{ color: "var(--th-fg-muted)" }}>{cmd.label}</span>
            <span className="flex shrink-0 items-center gap-1.5">
              <kbd
                className="rounded border px-1.5 py-0.5 font-mono text-xs"
                style={{
                  borderColor: "var(--th-border)",
                  color: direct[cmd.id] ? "var(--th-fg)" : "var(--th-fg-muted)",
                  backgroundColor: "var(--th-tile-bg)",
                }}
              >
                {direct[cmd.id] ? formatChord(direct[cmd.id]) : "—"}
              </kbd>
              {prefixed[cmd.id] && (
                <kbd
                  className="rounded border px-1.5 py-0.5 font-mono text-xs"
                  style={{
                    borderColor: "var(--th-border)",
                    color: "var(--th-fg-muted)",
                    backgroundColor: "var(--th-tile-bg)",
                  }}
                  title="Prefixed binding (press the prefix first)"
                >
                  {formatChord(prefixKey)} {prefixed[cmd.id]}
                </kbd>
              )}
            </span>
          </div>
        ))}
      </Group>
    </>
  );
}

// ---------------------------------------------------------------------------
// Rules (WS-5b) — the event→action engine's config surface. Each rule runs an
// action when a supervised session's FR-012 status transitions into a target
// status. The list + CRUD live in store/rules; lib/rulesMount does the firing.
// This section is a minimal-but-complete builder: enable/disable, add/remove, and
// per-rule editors for the trigger (from→to status) and the action (kind + its
// params). Every edit is live (it writes straight to the store, which persists).
// ---------------------------------------------------------------------------

/** Which action kinds take a free-text param, and what that text means for each
 *  (so the input's label/placeholder reads right per kind). `null` = no text
 *  param (restart needs none beyond an optional command, handled below). */
const ACTION_TEXT_HINT: Record<ActionKind, { label: string; placeholder: string } | null> = {
  notify: { label: "Message", placeholder: "A session changed status." },
  sendText: { label: "Text to send", placeholder: "e.g. continue" },
  run: { label: "Command", placeholder: "e.g. npm test" },
  spawn: { label: "Startup command (optional)", placeholder: "e.g. claude --resume" },
  restart: { label: "Startup command (optional)", placeholder: "e.g. claude" },
};

/** Which action kinds spawn a NEW terminal, so they offer a cwd field. */
const ACTION_HAS_CWD: Record<ActionKind, boolean> = {
  notify: false,
  sendText: false,
  run: false,
  spawn: true,
  restart: true,
};

function RulesSection() {
  const rules = useRules((s) => s.rules);
  const add = useRules((s) => s.add);

  return (
    <>
      <Group
        title="Rules"
        description="Run an action when a supervised Claude session's status changes — e.g. open a terminal when a session ends, or ping you when one needs a permission. Rules react to live status transitions; a loop-guard caps how often each rule can fire for a session."
      >
        {rules.length === 0 ? (
          <p className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
            No rules yet. Add one to react to a session's status change.
          </p>
        ) : (
          <div className="flex flex-col gap-3">
            {rules.map((rule) => (
              <RuleCard key={rule.id} rule={rule} />
            ))}
          </div>
        )}
        <div className="mt-1">
          <Btn onClick={add} title="Add a new (disabled) rule to configure">
            + Add rule
          </Btn>
        </div>
      </Group>
    </>
  );
}

/** One editable rule: enable toggle + name, trigger (from→to status), action
 *  (kind + params), and a remove button. */
function RuleCard({ rule }: { rule: Rule }) {
  const toggle = useRules((s) => s.toggle);
  const remove = useRules((s) => s.remove);
  const update = useRules((s) => s.update);

  const textHint = ACTION_TEXT_HINT[rule.action.kind];
  const hasCwd = ACTION_HAS_CWD[rule.action.kind];

  return (
    <div
      className="flex flex-col gap-2.5 rounded border p-3"
      style={{ borderColor: "var(--th-border)", opacity: rule.enabled ? 1 : 0.7 }}
    >
      {/* Header: enable switch + editable name + remove. */}
      <div className="flex items-center gap-2.5">
        <Switch
          checked={rule.enabled}
          onChange={() => toggle(rule.id)}
          label={`Enable ${rule.name}`}
        />
        <input
          value={rule.name}
          onChange={(e) => update(rule.id, { name: e.target.value })}
          placeholder="Rule name"
          className="min-w-0 flex-1 rounded border bg-transparent px-2 py-1 text-sm"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
          aria-label="Rule name"
        />
        <button
          type="button"
          onClick={() => remove(rule.id)}
          className="shrink-0 rounded border px-2 py-1 text-xs hover:bg-neutral-700/30"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
          title="Delete this rule"
        >
          Remove
        </button>
      </div>

      {/* Trigger: when status goes from → to. */}
      <Row label="When status becomes">
        <div className="flex items-center gap-2">
          <ThemeSelect
            value={rule.trigger.from}
            onChange={(v) =>
              update(rule.id, {
                trigger: { ...rule.trigger, from: v as SessionStatus | "any" },
              })
            }
            title="Only fire when the previous status was this (Any = don't care)"
          >
            <Opt value="any">{statusLabel("any")}</Opt>
            {TRIGGER_STATUSES.map((s) => (
              <Opt key={s} value={s}>
                {statusLabel(s)}
              </Opt>
            ))}
          </ThemeSelect>
          <span style={{ color: "var(--th-fg-muted)" }}>→</span>
          <ThemeSelect
            value={rule.trigger.to}
            onChange={(v) =>
              update(rule.id, {
                trigger: { ...rule.trigger, to: v as SessionStatus },
              })
            }
            title="The status the session must enter for this rule to fire"
          >
            {TRIGGER_STATUSES.map((s) => (
              <Opt key={s} value={s}>
                {statusLabel(s)}
              </Opt>
            ))}
          </ThemeSelect>
        </div>
      </Row>

      {/* Action: kind + its params. */}
      <Row label="Do">
        <ThemeSelect
          value={rule.action.kind}
          onChange={(v) =>
            update(rule.id, {
              action: { ...rule.action, kind: v as ActionKind },
            })
          }
          title="The action to run when this rule fires"
        >
          {ACTION_KINDS.map((k) => (
            <Opt key={k} value={k}>
              {actionKindLabel(k)}
            </Opt>
          ))}
        </ThemeSelect>
      </Row>

      {textHint && (
        <Row label={textHint.label}>
          <input
            value={rule.action.text ?? ""}
            onChange={(e) =>
              update(rule.id, {
                action: { ...rule.action, text: e.target.value },
              })
            }
            placeholder={textHint.placeholder}
            className="w-full rounded border bg-transparent px-2 py-1 text-sm"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
            aria-label={textHint.label}
          />
        </Row>
      )}

      {hasCwd && (
        <Row label="Directory (optional)">
          <input
            value={rule.action.cwd ?? ""}
            onChange={(e) =>
              update(rule.id, {
                action: { ...rule.action, cwd: e.target.value },
              })
            }
            placeholder="Inherit the session's directory"
            className="w-full rounded border bg-transparent px-2 py-1 text-sm"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
            aria-label="Working directory"
          />
        </Row>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Hotkeys — a reference for the keys NOT owned by the rebindable keymap.
//
// The app command shortcuts (new/close terminal, cycle, tab-jump, zoom, prefix,
// command palette, focus-toggle) are now rebindable through the keymap and are
// listed LIVE in the Keyboard section, so they intentionally do NOT appear here —
// a hand-maintained copy would lie the moment a user rebinds one. What's left is
// the genuinely FIXED surface: a couple of app keys the keymap doesn't own
// (delete-session, settings, exit-fullscreen) and the xterm-intrinsic keys that
// live inside the terminal (copy/paste/scroll/select).
// ---------------------------------------------------------------------------
function HotkeysSection() {
  const groups: {
    title: string;
    description?: string;
    keys: [string, string][];
  }[] = [
    {
      title: "Fixed app shortcuts",
      description:
        "App keys the rebindable keymap doesn't own — see the Keyboard section for everything that IS rebindable.",
      keys: [
        ["Ctrl/Cmd + Shift + W", "Delete terminal from session (kills tmux)"],
        ["Ctrl/Cmd + ,", "Open / close Settings"],
        ["Esc", "Exit a fullscreen tile"],
      ],
    },
    {
      title: "Inside a terminal",
      description:
        "Handled by the terminal itself (xterm), not the keymap — these aren't rebindable.",
      keys: [
        ["Ctrl + C", "Copy selection (or send SIGINT when nothing is selected)"],
        ["Ctrl + V", "Paste"],
        ["Mouse wheel", "Scroll (tmux mouse mode is on)"],
        ["Shift + drag", "Select text"],
      ],
    },
  ];
  return (
    <>
      {groups.map((g) => (
        <Group key={g.title} title={g.title} description={g.description}>
          {g.keys.map(([combo, desc]) => (
            <div
              key={combo}
              className="flex items-center justify-between gap-3 text-sm"
            >
              <span style={{ color: "var(--th-fg-muted)" }}>{desc}</span>
              <kbd
                className="shrink-0 rounded border px-1.5 py-0.5 font-mono text-xs"
                style={{
                  borderColor: "var(--th-border)",
                  color: "var(--th-fg)",
                  backgroundColor: "var(--th-tile-bg)",
                }}
              >
                {combo}
              </kbd>
            </div>
          ))}
        </Group>
      ))}
    </>
  );
}

// ---------------------------------------------------------------------------
// Hooks — Claude Code lifecycle hook install/uninstall (moved here from the
// sidebar). The installed check runs when this section mounts (a deliberate
// navigation), so there's no repeated "checking..." flash.
// ---------------------------------------------------------------------------
function HooksSection() {
  const [installed, setInstalled] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    claudeHooksInstalled()
      .then((v) => alive && setInstalled(v))
      .catch(() => alive && setInstalled(false));
    return () => {
      alive = false;
    };
  }, []);
  // HookInstallPanel is self-contained (its own header/description/buttons), so
  // it's rendered directly rather than wrapped in a Group.
  return <HookInstallPanel agentBin="t-hub-agent" installed={installed} setInstalled={setInstalled} />;
}

// ---------------------------------------------------------------------------
// Updates — in-app auto-updater surface (feat/auto-updater).
//
// Mirrors the sibling "scribe" app's About → Updates UI: current version, a
// status line, Check / Install / View-releases buttons, a "last checked" line,
// and the two persisted toggles (auto-check + auto-install, the latter disabled
// when auto-check is off). The actual download/verify/install is the official
// Tauri updater plugin; relaunch() restarts the app afterward.
// ---------------------------------------------------------------------------

/** Hand a URL to the OS default browser. Primary path is the shell plugin's
 *  open() (already a dep + capability); on failure fall back to window.open,
 *  which WebView2 routes externally for a _blank target. Mirrors WebPreview. */
async function openReleasesPage(url: string): Promise<void> {
  try {
    const { open } = await import("@tauri-apps/plugin-shell");
    await open(url);
  } catch {
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      /* nothing more we can do from the frontend */
    }
  }
}

function UpdatesSection() {
  const autoCheckEnabled = useSettings((s) => s.autoUpdateCheckEnabled);
  const setAutoCheckEnabled = useSettings((s) => s.setAutoUpdateCheckEnabled);
  const autoInstallUpdates = useSettings((s) => s.autoInstallUpdates);
  const setAutoInstallUpdates = useSettings((s) => s.setAutoInstallUpdates);

  const [version, setVersion] = useState<string>(pkg.version);
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [result, setResult] = useState<UpdateCheckResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [installStatus, setInstallStatus] = useState<string | null>(null);
  const [lastChecked, setLastChecked] = useState<number | null>(null);

  // Live runtime version (falls back to the build-time package.json version when
  // not running inside Tauri).
  useEffect(() => {
    let disposed = false;
    getVersion()
      .then((v) => {
        if (!disposed) setVersion(v);
      })
      .catch(() => {
        // Not inside Tauri — keep the package.json fallback.
      });
    return () => {
      disposed = true;
    };
  }, []);

  const handleCheck = async () => {
    if (checking) return;
    setChecking(true);
    setError(null);
    setResult(null);
    try {
      const r = await detectUpdate();
      setResult(r);
      setLastChecked(Date.now());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setChecking(false);
    }
  };

  const handleInstall = async () => {
    if (installing) return;
    setInstalling(true);
    setError(null);
    setInstallStatus("Contacting the release server...");
    try {
      const update = await checkUpdaterPackage();
      if (!update) {
        setInstallStatus(null);
        setError(
          "The latest release has no signed update package — use View releases to download the installer.",
        );
        return;
      }
      let downloaded = 0;
      let total: number | null = null;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? null;
            setInstallStatus("Downloading update...");
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            setInstallStatus(
              total
                ? `Downloading update... ${Math.round((downloaded / total) * 100)}%`
                : "Downloading update...",
            );
            break;
          case "Finished":
            setInstallStatus("Installing... the app will restart.");
            break;
        }
      });
      // On Windows the installer typically restarts the app itself, so execution
      // rarely gets past downloadAndInstall; relaunch covers the paths where it
      // does (and other platforms).
      await relaunch();
    } catch (cause) {
      setInstallStatus(null);
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setInstalling(false);
    }
  };

  const status =
    error ??
    installStatus ??
    (result
      ? result.updateAvailable
        ? `You're on an old version — v${result.latestVersion} is available.`
        : "You're on the latest version."
      : "Check this install against the latest GitHub release.");

  return (
    <>
      <Group title="Updates" description="Which build of T-Hub you're running, and how to get the newest one.">
        <Row label="Version">
          <span className="font-mono text-xs" style={{ color: "var(--th-fg)" }}>
            {version}
          </span>
        </Row>

        <div className="text-sm" style={{ color: "var(--th-fg-muted)" }}>
          {status}
        </div>
        <div className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
          {autoCheckEnabled ? "Checks automatically" : "Automatic checks are off"}
          {lastChecked
            ? ` · last checked ${new Date(lastChecked).toLocaleTimeString()}`
            : ""}
        </div>

        <div className="mt-1 flex flex-wrap items-center gap-1.5">
          {result?.updateAvailable && (
            <PrimaryBtn
              onClick={() => void handleInstall()}
              disabled={installing}
              title={`Download and install v${result.latestVersion}, then restart`}
            >
              {installing ? "Installing…" : `Install v${result.latestVersion}`}
            </PrimaryBtn>
          )}
          <Btn
            onClick={() => void handleCheck()}
            title="Check GitHub Releases for a newer version"
          >
            {checking ? "Checking…" : "Check for updates"}
          </Btn>
          <Btn
            onClick={() => void openReleasesPage(RELEASES_URL)}
            title="Open the GitHub releases page (every version + its notes)"
          >
            View releases
          </Btn>
        </div>
      </Group>

      <Group title="Automatic updates">
        <SettingToggleRow
          label="Automatically check for updates"
          hint="Periodically and on launch, look for a newer signed release. Manual 'Check for updates' still works when this is off."
          value={autoCheckEnabled}
          onChange={setAutoCheckEnabled}
        />
        <SettingToggleRow
          label="Install updates automatically"
          hint="When a new version is found on launch, download and install it silently, then restart — no Windows installer popups. Requires automatic checks."
          value={autoInstallUpdates}
          onChange={(v) => {
            // Inert while auto-check is off (the toggle is also visually
            // disabled below), so the two settings can't contradict.
            if (autoCheckEnabled) setAutoInstallUpdates(v);
          }}
          disabled={!autoCheckEnabled}
        />
      </Group>
    </>
  );
}

// ---------------------------------------------------------------------------
// About & Setup — what T-Hub is + a short how-it-works tutorial.
// ---------------------------------------------------------------------------
function AboutSection() {
  return (
    <>
      <AboutGroup />
      <Group
        title="What is T-Hub"
        description="A local, terminal-first cockpit for running and supervising many Claude Code sessions at once — free and open source, by n8builds. Windows + WSL."
      >
        <Bullet>Every terminal is a persistent tmux session — closing a tile detaches it; the session keeps running and can be re-adopted.</Bullet>
        <Bullet>Drag, resize, and reorder tiles freely — terminals never reload when they move.</Bullet>
        <Bullet>Install the Claude hooks to light up the supervision tree, the attention queue, and live context/cost/usage.</Bullet>
      </Group>
    </>
  );
}

function SetupSection() {
  return (
    <Group title="Quick start">
      <Step n={1}>Hit the “+” (bottom-right) or Ctrl/Cmd+T to open a terminal. Pick Shell, or Resume Claude to reopen a past session.</Step>
      <Step n={2}>Drag a tile’s header to rearrange the grid; drag a column/row gutter to resize. Drag a tile onto a workspace tab to move it there.</Step>
      <Step n={3}>Right-click a tile (or hold Shift over its “×”) to close or delete it; a plain “×” detaches (the session keeps running).</Step>
      <Step n={4}>Open the Files panel to browse the focused terminal’s project; click a file to preview or edit it.</Step>
      <Step n={5}>Install Claude hooks in Settings → Hooks (pick which events) to get the supervision tree, attention queue, and live usage.</Step>
    </Group>
  );
}

/** A simple bulleted line for the About/Setup copy. */
function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex gap-2 text-sm" style={{ color: "var(--th-fg-muted)" }}>
      <span style={{ color: "var(--th-accent)" }}>•</span>
      <span className="leading-snug">{children}</span>
    </div>
  );
}

/** A numbered step for the Quick start tutorial. */
function Step({ n, children }: { n: number; children: React.ReactNode }) {
  return (
    <div className="flex gap-2.5 text-sm">
      <span
        className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-xs font-semibold"
        style={{ backgroundColor: "var(--th-tile-bg)", color: "var(--th-fg)" }}
      >
        {n}
      </span>
      <span className="leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        {children}
      </span>
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
/** One row of the status-indicator legend: the live indicator on the right (so it
 *  always matches the real thing), with its name + meaning on the left. A `null`
 *  variant is the BLANK/empty state — shown as a faint dashed ring stand-in so the
 *  legend entry isn't an empty box. */
function StatusLegendRow({
  variant,
  name,
  hint,
}: {
  variant: StatusVariant | null;
  name: string;
  hint: string;
}) {
  return (
    <div className="flex items-start justify-between gap-3 text-sm">
      <span className="flex min-w-0 flex-col">
        <span style={{ color: "var(--th-fg)" }}>{name}</span>
        <span
          className="mt-1 text-xs leading-snug"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {hint}
        </span>
      </span>
      <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center">
        {variant === null ? (
          <span
            className="inline-block rounded-full"
            style={{
              width: 13,
              height: 13,
              border: "1px dashed var(--th-fg-muted)",
              opacity: 0.5,
            }}
            aria-label="Empty (no indicator)"
            title="Empty (no indicator)"
          />
        ) : (
          <StatusIndicator variant={variant} size={13} />
        )}
      </span>
    </div>
  );
}

/** An accent-filled call-to-action button (e.g. "Install vX"). Same shape as
 *  {@link Btn} but tinted with the theme accent to read as the primary action. */
function PrimaryBtn({
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
      className="rounded px-2.5 py-1.5 text-sm font-medium transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
      style={{ backgroundColor: "var(--th-accent)", color: "var(--th-fg)" }}
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
