// Consent-gated management for Codex's native lifecycle hooks.
//
// Codex keeps hook definitions and their trust state separate. T-Hub may safely
// merge and repair its marker-tagged entries in ~/.codex/hooks.json, but only
// Codex's own /hooks review flow may approve those commands. This panel keeps
// that boundary explicit and turns each backend health state into a concrete
// recovery action.
import { type ReactNode, useEffect, useState } from "react";
import {
  codexHooksHealth,
  installCodexHooks,
  repairCodexHooks,
  uninstallCodexHooks,
} from "../ipc/client05";
import type {
  CodexHookStatus,
  CodexHooksHealth,
  CodexHooksInstallReport,
} from "../ipc/model";

export interface CodexHookInstallPanelProps {
  /** Resolved WSL path to the t-hub-agent Codex hook entrypoint. */
  agentBin: string;
  /** Optional focused project root, used only to report project hook presence. */
  projectRoot?: string | null;
}

type MutatingAction = "install" | "repair" | "uninstall";

const STATUS_COPY: Record<
  CodexHookStatus,
  { label: string; tone: "good" | "warn" | "bad" | "muted"; summary: string }
> = {
  notInstalled: {
    label: "not installed",
    tone: "muted",
    summary: "T-Hub has not added lifecycle handlers to your Codex hook file.",
  },
  needsReview: {
    label: "needs review",
    tone: "warn",
    summary: "The handlers are installed, but Codex has not trusted all of them yet.",
  },
  healthy: {
    label: "healthy",
    tone: "good",
    summary: "All managed lifecycle handlers are installed, trusted, and enabled.",
  },
  disabled: {
    label: "disabled",
    tone: "warn",
    summary: "Codex is not currently allowing all managed handlers to run.",
  },
  modified: {
    label: "modified",
    tone: "warn",
    summary: "A managed command no longer matches the command Codex trusted.",
  },
  drifted: {
    label: "repair needed",
    tone: "bad",
    summary: "A managed event, command, or executable has drifted from this T-Hub build.",
  },
  blockedByManagedPolicy: {
    label: "blocked by policy",
    tone: "bad",
    summary: "Your organization allows only centrally managed hooks, so user hooks cannot run.",
  },
};

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export function CodexHookInstallPanel({
  agentBin,
  projectRoot = null,
}: CodexHookInstallPanelProps) {
  const [health, setHealth] = useState<CodexHooksHealth | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<MutatingAction | null>(null);
  const [consent, setConsent] = useState(false);
  const [report, setReport] = useState<CodexHooksInstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      setHealth(await codexHooksHealth(agentBin, projectRoot));
    } catch (cause) {
      setHealth(null);
      setError(errorMessage(cause));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    codexHooksHealth(agentBin, projectRoot)
      .then((next) => {
        if (alive) setHealth(next);
      })
      .catch((cause) => {
        if (!alive) return;
        setHealth(null);
        setError(errorMessage(cause));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [agentBin, projectRoot]);

  const mutate = async (action: MutatingAction) => {
    setBusy(action);
    setError(null);
    setReport(null);
    try {
      const next =
        action === "install"
          ? await installCodexHooks(agentBin, consent)
          : action === "repair"
            ? await repairCodexHooks(agentBin, consent)
            : await uninstallCodexHooks(agentBin);
      setReport(next);
      setHealth(next.health);
      if (action !== "uninstall") setConsent(false);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  };

  const status = health?.status ?? null;
  const statusCopy = status ? STATUS_COPY[status] : null;
  const installed = status !== null && status !== "notInstalled";
  const expectedEventCount = health
    ? health.managedEvents.length + health.missingEvents.length
    : 0;
  const canRepair =
    status === "drifted" &&
    health?.executableOk === true &&
    health.agentCapable === true &&
    health.hooksEnabled === true &&
    health.managedOnlyPolicy === false;
  const canInstall =
    status === "notInstalled" &&
    health?.executableOk === true &&
    health.agentCapable === true &&
    health.hooksEnabled === true &&
    health.managedOnlyPolicy === false;
  const needsConsent = canInstall || canRepair;

  return (
    <section
      className="flex flex-col gap-3 rounded border p-3 text-sm"
      style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
      aria-labelledby="codex-hooks-title"
    >
      <div className="flex items-center gap-2">
        <h3 id="codex-hooks-title" className="font-semibold">
          Codex hooks
        </h3>
        <CodexStatusPill health={health} loading={loading} />
        <button
          type="button"
          onClick={() => void refresh()}
          disabled={loading || busy !== null}
          className="ml-auto rounded border px-2 py-0.5 text-[11px] disabled:opacity-40"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
        >
          {loading ? "Checking..." : "Refresh status"}
        </button>
      </div>

      <p className="text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Adds native lifecycle handlers to your WSL <code>~/.codex/hooks.json</code>.
        These handlers carry Codex session, prompt, permission, question,
        completion, and end events into T-Hub for status and voice announcements.
        Other hooks and Codex settings are preserved.
      </p>

      {statusCopy && (
        <p className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
          {statusCopy.summary}
        </p>
      )}

      {health && !health.executableOk && (
        <Notice tone="bad" title="WSL helper unavailable">
          <code>{health.executablePath}</code> is missing or is not executable.
          Update or reinstall T-Hub's WSL helper before installing or repairing
          Codex hooks.
        </Notice>
      )}

      {health?.executableOk && !health.agentCapable && (
        <Notice tone="bad" title="WSL helper needs an update">
          The installed helper
          {health.agentVersion ? ` (${health.agentVersion})` : ""} does not
          advertise native Codex hook support. Update or reinstall T-Hub's WSL
          helper before installing or repairing these hooks.
        </Notice>
      )}

      {status === "needsReview" && (
        <TrustReviewNotice managedEvents={health?.managedEvents ?? []} />
      )}

      {health && !health.hooksEnabled && (
        <Notice tone="warn" title="Enable Codex hooks">
          Codex's hooks feature is globally off. Set <code>[features].hooks = true</code>{" "}
          in <code>~/.codex/config.toml</code>. If requirements or managed
          configuration enforce the disabled state, ask your Codex administrator
          to enable hooks. Then return here and select Refresh status.
        </Notice>
      )}

      {status === "disabled" && health?.hooksEnabled && (
        <Notice tone="warn" title="Enable the handlers in Codex">
          Open a Codex terminal, run <code>/hooks</code>, and enable each T-Hub
          handler. Then return here and select Refresh status.
        </Notice>
      )}

      {status === "modified" && (
        <Notice tone="warn" title="Review the changed commands">
          Open a Codex terminal and run <code>/hooks</code>. Review every T-Hub
          entry whose command contains <code>--codex-hook</code>, approve the
          current command, and then refresh this status.
        </Notice>
      )}

      {health?.inlineUserHooksPresent && (
        <Notice tone="warn" title="Review legacy inline hooks">
          Codex also found lifecycle hooks in <code>~/.codex/config.toml</code>.
          T-Hub preserves them, but redundant handlers can produce duplicate status
          and voice events. After the T-Hub entries are healthy, use{" "}
          <code>/hooks</code> to identify and migrate only the redundant inline
          handlers.
        </Notice>
      )}

      {health?.managedOnlyPolicy && (
        <Notice tone="bad" title="Organization policy blocks user hooks">
          This Codex installation accepts only centrally managed hooks. Ask your
          Codex administrator to allow or deploy the T-Hub lifecycle handlers.
          Local install or repair cannot override this policy.
        </Notice>
      )}

      {health && (
        <details
          className="rounded border px-2.5 py-2 text-xs"
          style={{ borderColor: "var(--th-border)" }}
        >
          <summary className="cursor-pointer" style={{ color: "var(--th-fg-muted)" }}>
            Installation details
          </summary>
          <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
            <Detail
              label="Managed events"
              value={`${health.managedEvents.length}/${expectedEventCount}`}
            />
            <Detail
              label="Executable"
              value={health.executableOk ? "available" : "missing or not executable"}
            />
            <Detail
              label="Native hook support"
              value={health.agentCapable ? "available" : "unsupported"}
            />
            <Detail
              label="Codex hooks feature"
              value={health.hooksEnabled ? "enabled" : "disabled"}
            />
            {health.agentVersion && (
              <Detail label="Helper version" value={health.agentVersion} code />
            )}
            <Detail label="Hook file" value={health.hooksPath} code />
            <Detail label="Codex config" value={health.configPath} code />
            {health.missingEvents.length > 0 && (
              <Detail label="Missing events" value={health.missingEvents.join(", ")} code />
            )}
            {health.projectHooksPresent && (
              <Detail label="Project hooks" value="also present and preserved" />
            )}
            {health.inlineUserHooksPresent && (
              <Detail label="Legacy inline hooks" value="present and preserved" />
            )}
            {health.pluginConfigPresent && (
              <Detail label="Plugin configuration" value="present and preserved" />
            )}
            {health.managedHooksPresent && (
              <Detail label="Centrally managed hooks" value="present and preserved" />
            )}
            <Detail
              label="Session start"
              value={health.capabilities.sessionStart}
              code
            />
            <Detail
              label="User prompt"
              value={health.capabilities.userPrompt}
              code
            />
            <Detail
              label="Permission"
              value={health.capabilities.permission}
              code
            />
            <Detail
              label="Completion"
              value={health.capabilities.completion}
              code
            />
            <Detail
              label="Session end"
              value={health.capabilities.sessionEnd}
              code
            />
            <Detail label="Question" value={health.capabilities.question} code />
            <Detail label="Failure" value={health.capabilities.failure} code />
          </dl>
        </details>
      )}

      {needsConsent && (
        <label
          className="flex items-start gap-2 text-xs"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <input
            type="checkbox"
            checked={consent}
            onChange={(event) => setConsent(event.target.checked)}
          />
          <span>
            I consent to editing my global <code>~/.codex/hooks.json</code> inside
            WSL.
          </span>
        </label>
      )}

      <div className="flex flex-wrap gap-2">
        {status === "notInstalled" && (
          <ActionButton
            disabled={!consent || busy !== null || !canInstall}
            onClick={() => void mutate("install")}
          >
            {busy === "install" ? "Installing..." : "Install Codex hooks"}
          </ActionButton>
        )}
        {canRepair && (
          <ActionButton
            disabled={!consent || busy !== null}
            onClick={() => void mutate("repair")}
          >
            {busy === "repair" ? "Repairing..." : "Repair Codex hooks"}
          </ActionButton>
        )}
        {installed && (
          <ActionButton
            danger
            disabled={busy !== null}
            onClick={() => void mutate("uninstall")}
          >
            {busy === "uninstall" ? "Removing..." : "Uninstall Codex hooks"}
          </ActionButton>
        )}
      </div>

      {needsConsent && !consent && health?.executableOk === true && (
        <p className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
          Tick the consent box to enable this configuration change.
        </p>
      )}

      {report && (
        <Notice
          tone="good"
          title={
            report.health.status === "notInstalled"
              ? "Codex hooks removed"
              : report.changed
                ? "Codex hook file updated"
                : "Codex hook file already matched"
          }
        >
          {report.managedEvents} managed handler
          {report.managedEvents === 1 ? "" : "s"} in{" "}
          <code>{report.hooksPath}</code>
          {report.backedUp ? ". The previous file was backed up." : "."}
        </Notice>
      )}

      {error && (
        <Notice tone="bad" title="Codex hook operation failed" role="alert">
          <span className="break-all">{error}</span>
        </Notice>
      )}
    </section>
  );
}

function TrustReviewNotice({ managedEvents }: { managedEvents: string[] }) {
  return (
    <Notice tone="warn" title="One Codex review is still required">
      T-Hub cannot approve its own hook commands. Open a Codex terminal, run{" "}
      <code>/hooks</code>, and review the {managedEvents.length} T-Hub{" "}
      {managedEvents.length === 1 ? "entry" : "entries"} whose commands contain{" "}
      <code>--codex-hook</code>. Trust and enable each entry, then return here and
      select Refresh status. Voice and lifecycle updates remain inactive until
      Codex reports this status as healthy.
    </Notice>
  );
}

function CodexStatusPill({
  health,
  loading,
}: {
  health: CodexHooksHealth | null;
  loading: boolean;
}) {
  if (loading) {
    return (
      <span className="rounded-full bg-neutral-800 px-2 py-0.5 text-[11px] text-neutral-400">
        checking...
      </span>
    );
  }
  if (!health) {
    return (
      <span className="rounded-full bg-red-950/60 px-2 py-0.5 text-[11px] text-red-300">
        unavailable
      </span>
    );
  }
  const copy = STATUS_COPY[health.status];
  const colors =
    copy.tone === "good"
      ? "bg-emerald-900/50 text-emerald-300"
      : copy.tone === "warn"
        ? "bg-amber-900/50 text-amber-200"
        : copy.tone === "bad"
          ? "bg-red-950/60 text-red-300"
          : "bg-neutral-800 text-neutral-400";
  return (
    <span className={`rounded-full px-2 py-0.5 text-[11px] ${colors}`}>
      {copy.label}
    </span>
  );
}

function ActionButton({
  children,
  danger = false,
  disabled,
  onClick,
}: {
  children: ReactNode;
  danger?: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={`rounded border border-neutral-700 bg-neutral-900 px-3 py-1 text-xs text-neutral-200 disabled:opacity-40 ${
        danger
          ? "enabled:hover:border-red-600 enabled:hover:text-white"
          : "enabled:hover:border-emerald-600 enabled:hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}

function Notice({
  title,
  tone,
  children,
  role,
}: {
  title: string;
  tone: "good" | "warn" | "bad";
  children: ReactNode;
  role?: "alert";
}) {
  const color =
    tone === "good"
      ? "var(--th-accent, #34d399)"
      : tone === "warn"
        ? "#fbbf24"
        : "var(--th-danger, #f87171)";
  return (
    <div
      className="rounded border p-2 text-xs leading-snug"
      style={{
        borderColor: color,
        background: "var(--th-bg-elevated, #0a0a0a)",
        color: "var(--th-fg)",
      }}
      role={role}
    >
      <div className="font-medium" style={{ color }}>
        {title}
      </div>
      <div className="mt-0.5" style={{ color: "var(--th-fg-muted)" }}>
        {children}
      </div>
    </div>
  );
}

function Detail({
  label,
  value,
  code = false,
}: {
  label: string;
  value: string;
  code?: boolean;
}) {
  return (
    <>
      <dt style={{ color: "var(--th-fg-muted)" }}>{label}</dt>
      <dd className="min-w-0 break-all" style={{ color: "var(--th-fg)" }}>
        {code ? <code>{value}</code> : value}
      </dd>
    </>
  );
}
