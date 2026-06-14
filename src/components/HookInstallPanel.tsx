// Consent-gated Claude hook install panel (Workstream B UI). Surfaces whether
// TermHub's hooks are installed in ~/.claude/settings.json and offers an
// explicit, consented install / uninstall. The REVIEW hard requirement is that
// editing the user's Claude config requires explicit consent and a clean
// uninstall — this panel makes both first-class and shows exactly what changed.
//
// `agentBin` is the resolved WSL path to the termhub-agent binary (the hook
// entrypoint, `termhub-agent --hook <EVENT>`). The caller supplies it.
import { useCallback, useEffect, useState } from "react";
import {
  claudeHooksInstalled,
  installClaudeHooks,
  uninstallClaudeHooks,
} from "../ipc/client05";
import type { InstallReport } from "../ipc/model";

export interface HookInstallPanelProps {
  /** Resolved WSL path to the termhub-agent binary (hook entrypoint). */
  agentBin: string;
}

// The 15 Claude Code lifecycle events TermHub registers. Each event gets a
// handler that runs `<agentBin> --hook <EVENT>` in ~/.claude/settings.json.
const HOOK_EVENTS = [
  "SessionStart",
  "SessionEnd",
  "UserPromptSubmit",
  "Stop",
  "StopFailure",
  "PermissionRequest",
  "Notification",
  "Elicitation",
  "SubagentStart",
  "SubagentStop",
  "TaskCreated",
  "TaskCompleted",
  "CwdChanged",
  "WorktreeCreate",
  "WorktreeRemove",
] as const;

export function HookInstallPanel({ agentBin }: HookInstallPanelProps) {
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<InstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showDetails, setShowDetails] = useState(false);

  const refresh = useCallback(() => {
    claudeHooksInstalled()
      .then(setInstalled)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const doInstall = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await installClaudeHooks(agentBin, consent);
      setReport(r);
      setInstalled(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [agentBin, consent]);

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
  }, []);

  return (
    <div className="flex flex-col gap-1.5 p-2 text-sm text-neutral-200">
      <div className="flex items-center gap-2">
        <span className="font-semibold">Claude hooks</span>
        <StatusPill installed={installed} />
      </div>
      <p className="text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Adds 15 lifecycle hook handlers to your{" "}
        <span style={{ color: "var(--th-fg)" }}>WSL</span>{" "}
        <code>~/.claude/settings.json</code> (where Claude Code actually reads
        them) &mdash; <span style={{ color: "var(--th-fg)" }}>global</span>, so
        it affects every Claude Code session in the distro, not just the focused
        terminal. Existing settings are preserved; uninstall removes only
        TermHub&apos;s entries.
      </p>

      <button
        type="button"
        onClick={() => setShowDetails((v) => !v)}
        className="self-start text-[11px] text-neutral-500 hover:text-neutral-300"
      >
        {showDetails ? "Hide details" : "View details"}
      </button>
      {showDetails && (
        <div
          className="rounded border border-neutral-800 bg-neutral-950 p-2 text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <ul className="flex flex-col gap-0.5 font-mono">
            {HOOK_EVENTS.map((event) => (
              <li key={event} className="truncate">
                {agentBin} --hook {event}
              </li>
            ))}
          </ul>
          <div className="mt-1.5 font-sans text-neutral-600">
            Written to <code>~/.claude/settings.json</code>.
          </div>
        </div>
      )}

      {!installed && (
        <label className="flex items-center gap-2 text-xs text-neutral-400">
          <input
            type="checkbox"
            checked={consent}
            onChange={(e) => setConsent(e.target.checked)}
          />
          <span>
            I consent to editing my global{" "}
            <code>~/.claude/settings.json</code> (inside WSL).
          </span>
        </label>
      )}

      <div className="flex gap-2">
        {!installed ? (
          <button
            type="button"
            disabled={!consent || busy}
            onClick={() => void doInstall()}
            className="rounded border border-neutral-700 bg-neutral-900 px-3 py-1 text-xs text-neutral-200 enabled:hover:border-emerald-600 enabled:hover:text-white disabled:opacity-40"
          >
            {busy ? "Installing…" : "Install hooks"}
          </button>
        ) : (
          <button
            type="button"
            disabled={busy}
            onClick={() => void doUninstall()}
            className="rounded border border-neutral-700 bg-neutral-900 px-3 py-1 text-xs text-neutral-200 enabled:hover:border-red-600 enabled:hover:text-white disabled:opacity-40"
          >
            {busy ? "Removing…" : "Uninstall hooks"}
          </button>
        )}
      </div>

      {/* Why is the install button doing nothing? Make the consent gate obvious. */}
      {!installed && !consent && (
        <p className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
          Tick the consent box above to enable the install button — TermHub will
          not touch your Claude settings without it.
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
            {installed
              ? `Installed to ${report.settingsPath}`
              : "Hooks removed"}
          </div>
          <div className="mt-0.5" style={{ color: "var(--th-fg-muted)" }}>
            {report.managedEvents} hook{report.managedEvents === 1 ? "" : "s"}{" "}
            {installed ? "active" : "remaining"}
            {report.backedUp && " · existing settings backed up"}
          </div>
          <div
            className="mt-1 break-all font-mono text-[11px]"
            style={{ color: "var(--th-fg-faint, #6b7280)" }}
          >
            {report.settingsPath}
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
