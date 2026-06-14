// Consent-gated, CUSTOMIZABLE Claude hook install panel (lives in Settings →
// Hooks). Editing the user's Claude config requires explicit consent and a clean
// uninstall. The user picks exactly which lifecycle hooks to install via a
// checklist; "Apply" reconciles the managed set to that selection (unchecking an
// event uninstalls it), and "Uninstall all" removes every TermHub hook.
//
// `agentBin` is the resolved WSL path to the termhub-agent binary (the hook
// entrypoint, `termhub-agent --hook <EVENT>`). `installed`/`setInstalled` are
// owned by the parent so the section shows status without a "checking…" flash.
import { useCallback, useEffect, useState } from "react";
import {
  claudeHooksManaged,
  installClaudeHooks,
  uninstallClaudeHooks,
} from "../ipc/client05";
import type { InstallReport } from "../ipc/model";

export interface HookInstallPanelProps {
  /** Resolved WSL path to the termhub-agent binary (hook entrypoint). */
  agentBin: string;
  /** Installed state (any hooks managed), owned by the parent — checked once at
   *  mount so the section never flashes "checking…". `null` only during that
   *  initial check. */
  installed: boolean | null;
  /** Update the parent's installed state after an install/uninstall. */
  setInstalled: (v: boolean) => void;
}

/** The 15 Claude Code lifecycle events TermHub can register, each with a short
 *  description of what it powers, so the user can choose meaningfully. Order
 *  matches the backend HOOK_EVENTS. */
const HOOK_EVENTS: { event: string; desc: string }[] = [
  { event: "SessionStart", desc: "A Claude session starts" },
  { event: "SessionEnd", desc: "A session ends" },
  { event: "UserPromptSubmit", desc: "You submit a prompt — derives the tile's goal title" },
  { event: "Stop", desc: "Claude finishes a turn — 'done' state + notification" },
  { event: "StopFailure", desc: "A turn ends in failure — error alert" },
  { event: "PermissionRequest", desc: "Claude asks for permission — attention queue" },
  { event: "Notification", desc: "Claude posts a notification" },
  { event: "Elicitation", desc: "Claude asks you a question — attention queue" },
  { event: "SubagentStart", desc: "A subagent starts — supervision tree" },
  { event: "SubagentStop", desc: "A subagent finishes" },
  { event: "TaskCreated", desc: "A background task is created — outstanding count" },
  { event: "TaskCompleted", desc: "A background task completes" },
  { event: "CwdChanged", desc: "The working directory changes" },
  { event: "WorktreeCreate", desc: "A git worktree is created" },
  { event: "WorktreeRemove", desc: "A git worktree is removed" },
];
const ALL_EVENTS = HOOK_EVENTS.map((h) => h.event);

export function HookInstallPanel({
  agentBin,
  installed,
  setInstalled,
}: HookInstallPanelProps) {
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<InstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The user's selection. Defaults to ALL; once we learn what's currently
  // managed we pre-check exactly those (so re-applying preserves their choice).
  const [selected, setSelected] = useState<Set<string>>(() => new Set(ALL_EVENTS));

  useEffect(() => {
    let alive = true;
    claudeHooksManaged()
      .then((managed) => {
        if (alive && managed.length > 0) setSelected(new Set(managed));
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  const toggle = (event: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(event)) next.delete(event);
      else next.add(event);
      return next;
    });
  const selectAll = () => setSelected(new Set(ALL_EVENTS));
  const selectNone = () => setSelected(new Set());

  const apply = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const events = ALL_EVENTS.filter((e) => selected.has(e));
      const r = await installClaudeHooks(agentBin, consent, events);
      setReport(r);
      setInstalled(events.length > 0);
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
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [setInstalled]);

  const selectedCount = selected.size;

  return (
    <div className="flex flex-col gap-2.5 text-sm text-neutral-200">
      <div className="flex items-center gap-2">
        <span className="font-semibold">Claude hooks</span>
        <StatusPill installed={installed} />
      </div>
      <p className="text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Adds lifecycle hook handlers to your{" "}
        <span style={{ color: "var(--th-fg)" }}>WSL</span>{" "}
        <code>~/.claude/settings.json</code> (global — every Claude Code session
        in the distro). Pick exactly which events to register below; Apply
        reconciles to your selection (unchecking one removes it). Your other
        settings are preserved.
      </p>

      {/* Selection toolbar. */}
      <div className="flex items-center gap-2 text-xs">
        <span style={{ color: "var(--th-fg-muted)" }}>
          {selectedCount} of {ALL_EVENTS.length} selected
        </span>
        <span className="flex-1" />
        <button
          type="button"
          onClick={selectAll}
          className="rounded border px-2 py-0.5 hover:bg-neutral-700/30"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        >
          All
        </button>
        <button
          type="button"
          onClick={selectNone}
          className="rounded border px-2 py-0.5 hover:bg-neutral-700/30"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        >
          None
        </button>
      </div>

      {/* The checklist. */}
      <div
        className="th-scroll max-h-64 overflow-y-auto rounded border"
        style={{ borderColor: "var(--th-border)" }}
      >
        {HOOK_EVENTS.map(({ event, desc }) => (
          <label
            key={event}
            className="flex cursor-pointer items-start gap-2 border-b px-2.5 py-1.5 last:border-b-0 hover:bg-neutral-700/20"
            style={{ borderColor: "var(--th-border)" }}
          >
            <input
              type="checkbox"
              checked={selected.has(event)}
              onChange={() => toggle(event)}
              className="mt-0.5"
            />
            <span className="flex min-w-0 flex-col">
              <span className="font-mono text-xs" style={{ color: "var(--th-fg)" }}>
                {event}
              </span>
              <span className="text-[11px] leading-snug" style={{ color: "var(--th-fg-muted)" }}>
                {desc}
              </span>
            </span>
          </label>
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
              ? "Select at least one hook (or use Uninstall all)"
              : "Install exactly the selected hooks"
          }
        >
          {busy ? "Applying…" : `Apply (${selectedCount})`}
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
          Tick the consent box to enable Apply — TermHub won't touch your Claude
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
    </div>
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
