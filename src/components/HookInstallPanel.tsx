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

export function HookInstallPanel({ agentBin }: HookInstallPanelProps) {
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<InstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);

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
    <div className="flex flex-col gap-2 p-3 text-sm text-neutral-200">
      <div className="flex items-center gap-2">
        <span className="font-semibold">Claude hooks</span>
        <StatusPill installed={installed} />
      </div>
      <p className="text-xs text-neutral-500">
        TermHub installs handlers into{" "}
        <code className="text-neutral-400">~/.claude/settings.json</code> for the
        15 lifecycle hooks so it can track session status, subagents, and
        questions. Your existing hooks and settings are preserved; uninstall
        removes only TermHub&apos;s entries.
      </p>

      {!installed && (
        <label className="flex items-center gap-2 text-xs text-neutral-400">
          <input
            type="checkbox"
            checked={consent}
            onChange={(e) => setConsent(e.target.checked)}
          />
          I consent to TermHub editing my <code>~/.claude/settings.json</code>.
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

      {report && (
        <div className="rounded border border-neutral-800 bg-neutral-950 p-2 text-xs text-neutral-400">
          <div>{report.message}</div>
          <div className="mt-1 text-neutral-600">
            {report.settingsPath}
            {report.backedUp && " · backed up"}
          </div>
        </div>
      )}
      {error && <div className="text-xs text-red-400">{error}</div>}
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
