import { useCallback, useEffect, useRef, useState } from "react";
import {
  historyFocus,
  historyList,
  historyResume,
  type HistoryEntry,
  type HistoryListResult,
} from "../ipc/history";
import { runWhenIdle } from "../lib/windowInteraction";
import { useWorkspace } from "../store/workspace";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";

const LEGACY_RECENT_KEYS = ["th.recent.cache.v1", "th.recent.hidden.v2"];
const CLAUDE_ICON_STYLE = { color: "#D97757" } as const;

export interface HistoryListProps {
  onCount?: (count: number) => void;
}

function discardLegacyRecentState(): void {
  try {
    for (const key of LEGACY_RECENT_KEYS) localStorage.removeItem(key);
  } catch {
    // Storage may be unavailable. History remains backend-authoritative.
  }
}

function relativeTime(timestamp: string): string {
  const epoch = Date.parse(timestamp);
  if (!Number.isFinite(epoch)) return "";
  const seconds = Math.max(0, Math.floor((Date.now() - epoch) / 1000));
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  if (seconds < 2592000) return `${Math.floor(seconds / 86400)}d`;
  return `${Math.floor(seconds / 2592000)}mo`;
}

function newRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `history-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function HistoryList({ onCount }: HistoryListProps) {
  const [catalog, setCatalog] = useState<HistoryListResult | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshGeneration = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    try {
      const next = await historyList();
      if (generation !== refreshGeneration.current) return;
      setCatalog((current) =>
        current?.revision === next.revision ? current : next,
      );
      setError(null);
    } catch (reason) {
      if (generation !== refreshGeneration.current) return;
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (generation === refreshGeneration.current) setLoaded(true);
    }
  }, []);

  useEffect(() => {
    discardLegacyRecentState();
    void refresh();
    const onFocus = () => runWhenIdle(() => void refresh());
    const onHistoryChanged = () => void refresh();
    window.addEventListener("focus", onFocus);
    window.addEventListener("t-hub:history-changed", onHistoryChanged);
    return () => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("t-hub:history-changed", onHistoryChanged);
      refreshGeneration.current += 1;
    };
  }, [refresh]);

  const entries = catalog?.entries ?? [];
  useEffect(() => onCount?.(entries.length), [entries.length, onCount]);

  if (!loaded) {
    return (
      <div className="px-3 py-2 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        Loading...
      </div>
    );
  }

  if (!catalog && error) {
    return (
      <div
        className="px-3 py-2 text-xs"
        style={{ color: "var(--th-danger, #f87171)" }}
        role="alert"
      >
        History is unavailable.
      </div>
    );
  }

  const unhealthySources =
    catalog?.sources.filter((source) => source.status !== "ready") ?? [];

  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {unhealthySources.length > 0 && (
        <div
          className="mx-1 mb-1 rounded-md px-2 py-1 text-[10px]"
          style={{ color: "var(--th-fg-muted)", background: "var(--th-bg-subtle)" }}
          title={unhealthySources
            .map((source) => `${source.harness}: ${source.reason ?? source.status}`)
            .join("\n")}
          role="status"
        >
          Partial History - {unhealthySources.map((source) => source.harness).join(", ")}
        </div>
      )}
      {entries.length === 0 ? (
        <div className="px-1 py-2 text-sm" style={{ color: "var(--th-fg-muted)" }}>
          No conversations found.
        </div>
      ) : (
        entries.map((entry) => (
          <HistoryRow key={entry.historyId} entry={entry} onChanged={refresh} />
        ))
      )}
    </div>
  );
}

function HistoryRow({
  entry,
  onChanged,
}: {
  entry: HistoryEntry;
  onChanged: () => Promise<void>;
}) {
  const activeTabId = useWorkspace((state) => state.activeTabId);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const busyRef = useRef(false);
  const requestId = useRef<string | null>(null);
  const active = entry.continuityState === "active";
  const canFocus = active && entry.actions.focus.status === "supported";
  const canResume =
    entry.continuityState === "resumable" &&
    entry.actions.resume.status === "supported";
  const actionable = canFocus || canResume;

  const act = useCallback(async () => {
    if (busyRef.current || !actionable) return;
    busyRef.current = true;
    setBusy(true);
    setActionError(null);
    try {
      if (canFocus) {
        await historyFocus(entry.historyId);
      } else {
        requestId.current ??= newRequestId();
        await historyResume(entry.historyId, requestId.current, activeTabId);
        requestId.current = null;
        await onChanged();
      }
    } catch (reason) {
      // Keep the resume request ID on failure so an ambiguous retry cannot spawn
      // a second terminal. A later click replays or resolves the same request.
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }, [activeTabId, actionable, canFocus, entry.historyId, onChanged]);

  const ProviderIcon = entry.harness === "codex" ? CodexIcon : ClaudeIcon;
  const providerStyle = entry.harness === "claude" ? CLAUDE_ICON_STYLE : undefined;
  const age = relativeTime(entry.lastSeenAt);
  const stateLabel = active
    ? "Active"
    : entry.continuityState === "resumable"
      ? age
        ? `${age} ago`
        : "Resumable"
      : entry.continuityState === "archived"
        ? "Archived"
        : "Needs recovery";
  const context = [entry.projectName, entry.branch, stateLabel].filter(Boolean).join(" - ");
  const actionLabel = canFocus ? "Focus conversation" : "Resume conversation";
  const actionReason = canFocus
    ? entry.actions.focus.reason
    : entry.actions.resume.reason;

  return (
    <div
      className="group flex min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-neutral-800/25"
      title={[entry.cwd, entry.lastText].filter(Boolean).join("\n")}
      data-history-id={entry.historyId}
      data-continuity-state={entry.continuityState}
    >
      <ProviderIcon
        size={14}
        className="mt-0.5 shrink-0 self-start"
        style={providerStyle}
        title={entry.harness === "codex" ? "Codex" : "Claude"}
      />
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{entry.label}</div>
        <div
          className="truncate text-[11px]"
          style={{ color: active ? "var(--th-accent)" : "var(--th-fg-muted)" }}
        >
          {context || stateLabel}
        </div>
        {actionError && (
          <div
            className="truncate text-[10px]"
            style={{ color: "var(--th-danger, #f87171)" }}
            title={actionError}
            role="alert"
          >
            Action failed. Retry safely.
          </div>
        )}
      </div>
      <button
        type="button"
        onClick={() => void act()}
        disabled={busy || !actionable}
        className="shrink-0 rounded-md px-2 py-1.5 text-[14px] leading-none opacity-60 transition-opacity hover:bg-neutral-700/50 hover:opacity-100 focus:opacity-100 group-hover:opacity-100 disabled:cursor-not-allowed disabled:opacity-30"
        style={{ color: "var(--th-fg-muted)" }}
        aria-label={`${actionLabel}: ${entry.label}`}
        title={actionable ? actionLabel : actionReason ?? "Action unavailable"}
      >
        {busy ? "..." : canFocus ? "◎" : "→"}
      </button>
    </div>
  );
}
