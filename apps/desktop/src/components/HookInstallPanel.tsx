// Consent-gated, CATEGORY-BASED Claude hook install panel (lives in Settings →
// Hooks). Editing the user's Claude config requires explicit consent and a clean
// uninstall.
//
// Instead of a flat 15-row checklist (which read as a wall of opaque event
// names), the lifecycle hooks are grouped into a handful of OUTCOME CATEGORIES —
// "what does turning this on actually get me?" Each category is one toggle that
// enables/disables its whole set of underlying events, with a plain-English
// description and an expandable disclosure listing the exact event names and the
// `agentBin --hook <EVENT>` command each registers. "Apply" reconciles the
// managed set to the union of the enabled categories' events (turning a category
// off uninstalls its events); "Uninstall all" removes every T-Hub hook.
//
// `agentBin` is the resolved WSL path to the t-hub-agent binary (the hook
// entrypoint, `t-hub-agent --hook <EVENT>`). `installed`/`setInstalled` are
// owned by the parent so the section shows status without a "checking…" flash.
import { useCallback, useEffect, useState } from "react";
import {
  claudeHooksManaged,
  installClaudeHooks,
  uninstallClaudeHooks,
} from "../ipc/client05";
import type { InstallReport } from "../ipc/model";
// The Attention category surfaces the two notification controls (sounds +
// desktop notifications) right where they're relevant — the toggles live in the
// settings store, not in the hook set, but this is the obvious place to find
// them ("a session needs me" is an attention outcome).
import { useSettings } from "../store/settings";

export interface HookInstallPanelProps {
  /** Resolved WSL path to the t-hub-agent binary (hook entrypoint). */
  agentBin: string;
  /** Installed state (any hooks managed), owned by the parent — checked once at
   *  mount so the section never flashes "checking…". `null` only during that
   *  initial check. */
  installed: boolean | null;
  /** Update the parent's installed state after an install/uninstall. */
  setInstalled: (v: boolean) => void;
}

/** One lifecycle event T-Hub can register, with a short description of what it
 *  powers. The per-event copy is reused inside each category's disclosure. */
interface HookEvent {
  event: string;
  desc: string;
}

/** An outcome-oriented grouping of lifecycle events: a plain-English answer to
 *  "what do I get if I turn this on?". The whole category is one toggle that
 *  selects/deselects its `events`. Order of events within a category matches the
 *  backend HOOK_EVENTS order. */
interface HookCategory {
  id: string;
  title: string;
  /** One-line, plain-English summary of the outcome this category enables. */
  blurb: string;
  events: HookEvent[];
}

/** The 15 Claude Code lifecycle events, grouped into four outcome categories.
 *  Every event from the old flat list appears in exactly one category; the union
 *  of all four still equals the full backend HOOK_EVENTS set. */
const HOOK_CATEGORIES: HookCategory[] = [
  {
    id: "attention",
    title: "Attention & notifications",
    blurb:
      "Know the moment a session needs you — finished a turn, hit an error, or is asking for permission or a decision.",
    events: [
      { event: "Stop", desc: "Claude finishes a turn — 'done' state + notification" },
      { event: "StopFailure", desc: "A turn ends in failure — error alert" },
      { event: "PermissionRequest", desc: "Claude asks for permission — attention queue" },
      { event: "Notification", desc: "Claude posts a notification" },
      { event: "Elicitation", desc: "Claude asks you a question — attention queue" },
    ],
  },
  {
    id: "session",
    title: "Session tracking",
    blurb:
      "Follow each session's lifecycle — when it starts and ends, the prompt that drives its goal title, and where it's working.",
    events: [
      { event: "SessionStart", desc: "A Claude session starts" },
      { event: "SessionEnd", desc: "A session ends" },
      { event: "UserPromptSubmit", desc: "You submit a prompt — derives the tile's goal title" },
      { event: "CwdChanged", desc: "The working directory changes" },
    ],
  },
  {
    id: "supervision",
    title: "Supervision",
    blurb:
      "See the work fanning out underneath a session — its subagents and background tasks, with live outstanding counts.",
    events: [
      { event: "SubagentStart", desc: "A subagent starts — supervision tree" },
      { event: "SubagentStop", desc: "A subagent finishes" },
      { event: "TaskCreated", desc: "A background task is created — outstanding count" },
      { event: "TaskCompleted", desc: "A background task completes" },
    ],
  },
  {
    id: "worktrees",
    title: "Worktrees",
    blurb: "Track git worktrees as sessions create and remove them.",
    events: [
      { event: "WorktreeCreate", desc: "A git worktree is created" },
      { event: "WorktreeRemove", desc: "A git worktree is removed" },
    ],
  },
];

/** Every managed event, in backend order (the union across all categories). */
const ALL_EVENTS = HOOK_CATEGORIES.flatMap((c) => c.events.map((e) => e.event));
/** The category that owns a given event (for mapping the installed set → which
 *  categories read as on). */
function categoryEvents(cat: HookCategory): string[] {
  return cat.events.map((e) => e.event);
}

/** The marker T-Hub embeds in every command string it writes to settings.json,
 *  mirroring `T_HUB_HOOK_MARKER` in `src-tauri/src/claude/hooks.rs`; the
 *  uninstaller scans for it to remove exactly our entries. */
const T_HUB_HOOK_MARKER = "__t_hub_managed__";

/** Build the EXACT settings.json fragment T-Hub merges into
 *  `~/.claude/settings.json` for the given selection, client-side. This mirrors
 *  the Rust `t_hub_hooks_fragment_for` + `t_hub_statusline` shapes in
 *  `src-tauri/src/claude/hooks.rs` so the user sees the real code, not a
 *  paraphrase. `events` is ordered in backend HOOK_EVENTS order (ALL_EVENTS) and
 *  filtered to the current selection; an empty selection still shows the
 *  statusLine (T-Hub always installs it). */
function buildHooksJson(agentBin: string, events: string[]): Record<string, unknown> {
  const hooks: Record<string, unknown> = {};
  for (const event of events) {
    hooks[event] = [
      {
        matcher: "*",
        hooks: [
          {
            type: "command",
            command: `${agentBin} --hook ${event} # ${T_HUB_HOOK_MARKER}`,
          },
        ],
      },
    ];
  }
  return {
    hooks,
    statusLine: {
      type: "command",
      command: `${agentBin} --statusline # ${T_HUB_HOOK_MARKER}`,
      padding: 0,
      refreshInterval: 5,
    },
  };
}

export function HookInstallPanel({
  agentBin,
  installed,
  setInstalled,
}: HookInstallPanelProps) {
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<InstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The user's selection (what Apply will install), tracked per individual event
  // so a category toggle is just "are all my events selected?". Defaults to ALL;
  // once we learn what's currently managed we pre-select exactly those (an empty
  // managed set still defaults to all-on, matching today's behavior).
  const [selected, setSelected] = useState<Set<string>>(() => new Set(ALL_EVENTS));
  // Which events are CURRENTLY installed (for the per-row "installed" badge in a
  // category's disclosure), so it's clear what's live vs. what you're about to
  // change.
  const [installedEvents, setInstalledEvents] = useState<Set<string>>(new Set());
  // Whether the "View raw JSON" modal is open — it shows the literal settings.json
  // fragment T-Hub writes for the *current* selection (the real code, not a
  // description), built client-side and updated live as categories toggle.
  const [showRawJson, setShowRawJson] = useState(false);

  useEffect(() => {
    let alive = true;
    claudeHooksManaged()
      .then((managed) => {
        if (!alive) return;
        setInstalledEvents(new Set(managed));
        setSelected(managed.length > 0 ? new Set(managed) : new Set(ALL_EVENTS));
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  // Enable/disable a whole category: select (or deselect) every event it owns.
  const setCategory = (cat: HookCategory, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      for (const e of categoryEvents(cat)) {
        if (on) next.add(e);
        else next.delete(e);
      }
      return next;
    });

  const apply = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      // Reconcile to the union of the enabled categories' events (ALL_EVENTS is
      // already exactly that union, filtered by the per-event selection).
      const events = ALL_EVENTS.filter((e) => selected.has(e));
      const r = await installClaudeHooks(agentBin, consent, events);
      setReport(r);
      setInstalled(events.length > 0);
      setInstalledEvents(new Set(events)); // managed set is now exactly the selection
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [agentBin, consent, selected, setInstalled]);

  const doUninstall = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await uninstallClaudeHooks();
      setReport(r);
      setInstalled(false);
      setInstalledEvents(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [setInstalled]);

  const selectedCount = selected.size;
  // How many of the four categories are fully on, for the Apply summary line.
  const enabledCategories = HOOK_CATEGORIES.filter((c) =>
    categoryEvents(c).every((e) => selected.has(e)),
  ).length;

  return (
    <div className="flex flex-col gap-3 text-sm text-neutral-200">
      <div className="flex items-center gap-2">
        <span className="font-semibold">Claude hooks</span>
        <StatusPill installed={installed} />
        {/* "View raw JSON" — opens the literal settings.json fragment T-Hub
            writes for the current selection, so nothing is hidden behind the
            outcome blurbs. Pushed to the right so it reads as a peek action. */}
        <button
          type="button"
          onClick={() => setShowRawJson(true)}
          className="ml-auto rounded border px-2 py-0.5 text-[11px]"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
          title="Show the exact JSON T-Hub writes to ~/.claude/settings.json for your current selection"
        >
          View raw JSON
        </button>
      </div>
      <p className="text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Adds lifecycle hook handlers to your{" "}
        <span style={{ color: "var(--th-fg)" }}>WSL</span>{" "}
        <code>~/.claude/settings.json</code> (global — every Claude Code session
        in the distro). Pick the outcomes you want below; each one registers a
        small group of events. Apply reconciles to your selection (turning a
        category off removes its events). Your other settings are preserved.
      </p>

      {/* One card per outcome category: a header toggle + blurb, plus (for
          Attention) the notification controls, and an expandable list of the
          exact events/commands. */}
      <div className="flex flex-col gap-2">
        {HOOK_CATEGORIES.map((cat) => (
          <CategoryCard
            key={cat.id}
            cat={cat}
            agentBin={agentBin}
            selected={selected}
            installedEvents={installedEvents}
            onToggleCategory={(on) => setCategory(cat, on)}
          />
        ))}
      </div>

      <label className="flex items-center gap-2 text-xs" style={{ color: "var(--th-fg-muted)" }}>
        <input
          type="checkbox"
          checked={consent}
          onChange={(e) => setConsent(e.target.checked)}
        />
        <span>
          I consent to editing my global <code>~/.claude/settings.json</code> (inside WSL).
        </span>
      </label>

      <div className="flex gap-2">
        <button
          type="button"
          disabled={!consent || busy || selectedCount === 0}
          onClick={() => void apply()}
          className="rounded border border-neutral-700 bg-neutral-900 px-3 py-1 text-xs text-neutral-200 enabled:hover:border-emerald-600 enabled:hover:text-white disabled:opacity-40"
          title={
            selectedCount === 0
              ? "Enable at least one category (or use Uninstall all)"
              : "Install exactly the enabled categories' hooks"
          }
        >
          {busy
            ? "Applying…"
            : `Apply (${enabledCategories} ${enabledCategories === 1 ? "category" : "categories"})`}
        </button>
        {installed && (
          <button
            type="button"
            disabled={busy}
            onClick={() => void doUninstall()}
            className="rounded border border-neutral-700 bg-neutral-900 px-3 py-1 text-xs text-neutral-200 enabled:hover:border-red-600 enabled:hover:text-white disabled:opacity-40"
          >
            {busy ? "Removing…" : "Uninstall all"}
          </button>
        )}
      </div>

      {!consent && (
        <p className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
          Tick the consent box to enable Apply — T-Hub won't touch your Claude
          settings without it.
        </p>
      )}

      {report && (
        <div
          className="rounded border p-2 text-xs"
          style={{
            borderColor: "var(--th-accent, #34d399)",
            background: "var(--th-bg-elevated, #0a0a0a)",
            color: "var(--th-fg)",
          }}
        >
          <div className="font-medium" style={{ color: "var(--th-accent, #34d399)" }}>
            {report.managedEvents > 0
              ? `Installed to ${report.settingsPath}`
              : "Hooks removed"}
          </div>
          <div className="mt-0.5" style={{ color: "var(--th-fg-muted)" }}>
            {report.managedEvents} hook{report.managedEvents === 1 ? "" : "s"} active
            {report.backedUp && " · existing settings backed up"}
          </div>
        </div>
      )}
      {error && (
        <div
          className="rounded border p-2 text-xs"
          style={{
            borderColor: "var(--th-danger, #f87171)",
            background: "var(--th-bg-elevated, #0a0a0a)",
            color: "var(--th-danger, #f87171)",
          }}
        >
          <span className="font-medium">Hook install failed: </span>
          <span className="break-all">{error}</span>
        </div>
      )}

      {showRawJson && (
        <RawJsonModal
          agentBin={agentBin}
          events={ALL_EVENTS.filter((e) => selected.has(e))}
          onClose={() => setShowRawJson(false)}
        />
      )}
    </div>
  );
}

/** Modal showing the literal settings.json fragment T-Hub writes for the
 *  current selection — the real config (built by `buildHooksJson`, mirroring the
 *  Rust shapes), pretty-printed in a scrollable monospace block with a Copy
 *  button. `events` is the live, backend-ordered, selection-filtered event list,
 *  so the JSON reflects exactly what Apply would install right now. */
function RawJsonModal({
  agentBin,
  events,
  onClose,
}: {
  agentBin: string;
  events: string[];
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const json = JSON.stringify(buildHooksJson(agentBin, events), null, 2);

  // Close on Escape, matching the rest of the app's modals.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const copy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(json);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard can reject (permissions); silently no-op rather than throw.
    }
  }, [json]);

  return (
    // Backdrop: click outside the card closes; the card stops propagation.
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ background: "color-mix(in srgb, var(--th-bg) 70%, transparent)" }}
      onClick={onClose}
      role="presentation"
    >
      <div
        className="flex max-h-[80vh] w-full max-w-2xl flex-col overflow-hidden rounded-lg border shadow-xl"
        style={{ borderColor: "var(--th-border)", background: "var(--th-bg-elevated, #0a0a0a)" }}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Raw hook configuration JSON"
      >
        <div
          className="flex items-center gap-2 border-b px-3 py-2"
          style={{ borderColor: "var(--th-border)" }}
        >
          <span className="text-sm font-medium" style={{ color: "var(--th-fg)" }}>
            settings.json
          </span>
          <span className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
            {events.length} hook{events.length === 1 ? "" : "s"} + statusLine
          </span>
          <button
            type="button"
            onClick={() => void copy()}
            className="ml-auto rounded border px-2 py-0.5 text-[11px]"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            title="Copy this JSON to the clipboard"
          >
            {copied ? "Copied" : "Copy"}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="rounded border px-2 py-0.5 text-[11px]"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            aria-label="Close"
          >
            Close
          </button>
        </div>
        <p className="px-3 pt-2 text-[11px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
          The exact entries T-Hub merges into your{" "}
          <code>~/.claude/settings.json</code> for the current selection (your
          other settings are preserved). Updates live as you toggle categories.
        </p>
        {/* Opaque, scrollable monospace block so the real config is readable and
            copyable without truncation. */}
        <pre
          className="m-3 mt-2 overflow-auto rounded border p-3 text-[11px] leading-relaxed"
          style={{
            borderColor: "var(--th-border)",
            background: "var(--th-bg, #0a0a0a)",
            color: "var(--th-fg)",
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
          }}
        >
          {json}
        </pre>
      </div>
    </div>
  );
}

/** One outcome category, rendered as a bordered card: a header row (title +
 *  master toggle), the plain-English blurb, the optional notification controls
 *  (Attention only), and a "view hooks" disclosure with the exact events. The
 *  category reads as ON only when *every* event it owns is selected. */
function CategoryCard({
  cat,
  agentBin,
  selected,
  installedEvents,
  onToggleCategory,
}: {
  cat: HookCategory;
  agentBin: string;
  selected: Set<string>;
  installedEvents: Set<string>;
  onToggleCategory: (on: boolean) => void;
}) {
  // Collapsed by default — the outcome blurb is the headline; the underlying
  // event list is detail you open on demand.
  const [open, setOpen] = useState(false);
  const events = cat.events.map((e) => e.event);
  const enabled = events.every((e) => selected.has(e));
  const enabledCount = events.filter((e) => selected.has(e)).length;

  return (
    <div className="rounded border" style={{ borderColor: "var(--th-border)" }}>
      {/* Header: title + master toggle. The toggle is the only control that
          flips the category; clicking the title does nothing (matches the rest
          of Settings, where labels are inert). */}
      <div className="flex items-start justify-between gap-3 px-3 py-2.5">
        <div className="flex min-w-0 flex-col gap-0.5">
          <span className="font-medium" style={{ color: "var(--th-fg)" }}>
            {cat.title}
          </span>
          <span className="text-[11px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
            {cat.blurb}
          </span>
        </div>
        <span className="mt-0.5 shrink-0">
          <Toggle
            checked={enabled}
            onChange={onToggleCategory}
            label={`${cat.title} hooks`}
          />
        </span>
      </div>

      {/* Attention category: surface the notification controls right here, since
          "a session needs me" is the outcome these toggles serve. (The user
          could never find the sound toggle when it was buried in General.) */}
      {cat.id === "attention" && <NotificationControls />}

      {/* "View hooks" disclosure: the exact event names + the command each one
          registers, so nothing about what lands in settings.json is hidden. */}
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 border-t px-3 py-1.5 text-left"
        style={{ borderColor: "var(--th-border)" }}
        aria-expanded={open}
        title="Show the exact lifecycle events this category registers"
      >
        <Chevron open={open} />
        <span className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
          {open ? "Hide hooks" : "View hooks"}
        </span>
        <span className="tabular-nums text-[11px]" style={{ color: "var(--th-fg-muted)", opacity: 0.8 }}>
          ({enabledCount}/{events.length})
        </span>
      </button>
      {open && (
        <div className="border-t" style={{ borderColor: "var(--th-border)" }}>
          {cat.events.map(({ event, desc }) => (
            <div
              key={event}
              className="flex flex-col gap-0.5 border-b px-3 py-1.5 last:border-b-0"
              style={{ borderColor: "var(--th-border)" }}
            >
              <span className="flex items-center gap-2">
                <span className="font-mono text-xs" style={{ color: "var(--th-fg)" }}>
                  {event}
                </span>
                {installedEvents.has(event) && (
                  <span
                    className="rounded-full px-1.5 py-px text-[9px] font-semibold uppercase tracking-wide"
                    style={{
                      backgroundColor: "color-mix(in srgb, var(--th-dot-live) 22%, transparent)",
                      color: "var(--th-dot-live)",
                    }}
                    title="Currently installed"
                  >
                    installed
                  </span>
                )}
              </span>
              <span className="text-[11px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
                {desc}
              </span>
              {/* The exact command this hook runs (Claude Code runs it on the
                  event), so you can see what gets put in settings.json. */}
              <code
                className="block truncate text-[10px]"
                style={{ color: "var(--th-fg-muted)", opacity: 0.85 }}
                title={`${agentBin} --hook ${event}`}
              >
                {agentBin} --hook {event}
              </code>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** The two notification controls (sounds + desktop notifications), surfaced
 *  inside the Attention category. These are settings-store flags (not hook
 *  events) — flipping them doesn't change the managed hook set; they just gate
 *  the chime / OS notification when an attention event fires. */
function NotificationControls() {
  const soundsEnabled = useSettings((s) => s.soundsEnabled);
  const setSoundsEnabled = useSettings((s) => s.setSoundsEnabled);
  const notificationsEnabled = useSettings((s) => s.notificationsEnabled);
  const setNotificationsEnabled = useSettings((s) => s.setNotificationsEnabled);
  return (
    <div
      className="flex flex-col gap-2 border-t px-3 py-2.5"
      style={{ borderColor: "var(--th-border)" }}
    >
      <NotifyToggleRow
        label="Notification sounds"
        hint="Play a short chime on key session events — a soft cue when a session needs your input or finishes, and an alert when one errors out."
        value={soundsEnabled}
        onChange={setSoundsEnabled}
      />
      <NotifyToggleRow
        label="Desktop notifications"
        hint="Show an OS notification for the same session events. Requires the notification plugin; falls back to sound-only if it isn't available."
        value={notificationsEnabled}
        onChange={setNotificationsEnabled}
      />
    </div>
  );
}

/** A label + helper-text row with a switch on the right (mirrors the Settings
 *  `SettingToggleRow`), for the notification flags inside the Attention card.
 *  Only the switch flips the value; the label/helper are inert. */
function NotifyToggleRow({
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
          <span className="mt-0.5 text-[11px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
            {hint}
          </span>
        )}
      </span>
      <span className="mt-0.5 shrink-0">
        <Toggle checked={value} onChange={onChange} label={label} />
      </span>
    </div>
  );
}

/**
 * A small switch-style toggle drawn from theme vars (no external CSS), matching
 * the Settings `Switch`. A hidden native checkbox carries focus/accessibility
 * while the pill + knob render the visual state; the track tints to the accent
 * when on. Used for both the category master toggles and the notification flags.
 */
function Toggle({
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
      style={{ backgroundColor: checked ? "var(--th-accent)" : "var(--th-border)" }}
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

/** A small disclosure chevron; rotates when open (matches ThemeEditor). */
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

function StatusPill({ installed }: { installed: boolean | null }) {
  if (installed === null) {
    return <span className="text-xs text-neutral-600">checking…</span>;
  }
  return installed ? (
    <span className="rounded-full bg-emerald-900/50 px-2 py-0.5 text-[11px] text-emerald-300">
      installed
    </span>
  ) : (
    <span className="rounded-full bg-neutral-800 px-2 py-0.5 text-[11px] text-neutral-400">
      not installed
    </span>
  );
}
