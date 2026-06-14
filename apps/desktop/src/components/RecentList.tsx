// The sidebar's "Recent" list (feat/projects-sidebar, Agent A).
//
// Recent = past Claude Code sessions the user can RECALL. Each row is a session
// read from the on-disk Claude transcripts (via the `recent_sessions` IPC →
// recent.rs): its directory + a label (Claude's summary, else the cwd basename)
// + a last-seen time. Clicking a row RECALLS it: spawn a NEW terminal in that
// session's cwd running `claude --resume <id>`, add the tile to the active tab,
// and focus it (App wires `onRecall` to the workspace store's `recall` action,
// which reuses the normal spawn path).
//
// The list is fetched once on mount (and refetched when the window regains
// focus, so a session you just used elsewhere shows up). Best-effort: an IPC
// failure (no backend / non-Tauri context) degrades to an empty list with a
// muted hint, never an error surface.
import { useCallback, useEffect, useState } from "react";
import { recentSessions, type RecentSession } from "../ipc/recent";

export interface RecentListProps {
  /** Recall a past session: spawn `claude --resume <id>` in `cwd`, focus it. */
  onRecall: (sessionId: string, cwd: string) => void;
}

/** Final path segment of a cwd (POSIX or Windows separators), or "" if none —
 *  the "directory" shown faint under a session's label. Mirrors the backend's
 *  cwd_basename so the row's directory matches the label fallback. */
function cwdBasename(cwd: string): string {
  const parts = cwd.replace(/[/\\]+$/, "").split(/[/\\]+/);
  return parts[parts.length - 1] ?? "";
}

/**
 * Format an epoch-SECONDS timestamp as a compact relative time ("3m", "2h",
 * "5d") for the row's right-aligned last-seen hint. Falls back to "" for a
 * missing/zero stamp. Coarse by design — the list is sorted newest-first, so an
 * exact time isn't needed, just a sense of recency.
 */
function relativeTime(epochSecs: number): string {
  if (!epochSecs) return "";
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - epochSecs);
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 2592000) return `${Math.floor(diff / 86400)}d`;
  return `${Math.floor(diff / 2592000)}mo`;
}

export function RecentList({ onRecall }: RecentListProps) {
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  // null = still loading the first fetch; [] (loaded) = genuinely empty.
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(() => {
    // Skip work when the window is hidden (matches Canvas's cwd poll guard).
    if (typeof document !== "undefined" && document.visibilityState === "hidden") {
      return;
    }
    void recentSessions()
      .then((list) => {
        setSessions(list);
        setLoaded(true);
      })
      .catch(() => {
        // No backend / transient error: leave the current list, mark loaded so
        // we show the empty hint rather than a perpetual "Loading…".
        setLoaded(true);
      });
  }, []);

  // Fetch once on mount, and refetch when the window regains focus so a session
  // you just used in another window/terminal appears without a manual reload.
  useEffect(() => {
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  if (!loaded) {
    return (
      <div className="px-2 py-1 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        Loading…
      </div>
    );
  }
  if (sessions.length === 0) {
    return (
      <div className="px-2 py-1 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        No recent Claude sessions to recall.
      </div>
    );
  }
  return (
    <ul>
      {sessions.map((s) => (
        <RecentRow key={s.id} session={s} onRecall={onRecall} />
      ))}
    </ul>
  );
}

/** One recent session: a two-line cell (label over its directory) with a faint
 *  relative last-seen time on the right. Clicking recalls it. */
function RecentRow({
  session,
  onRecall,
}: {
  session: RecentSession;
  onRecall: (sessionId: string, cwd: string) => void;
}) {
  const dir = cwdBasename(session.cwd);
  const when = relativeTime(session.lastSeen);
  return (
    <li>
      <button
        type="button"
        onClick={() => onRecall(session.id, session.cwd)}
        className="flex w-full cursor-pointer items-center gap-2 py-1 pr-2 pl-2 text-left hover:bg-neutral-900"
        style={{ color: "var(--th-fg)" }}
        title={`Recall: claude --resume in ${session.cwd}`}
      >
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm">{session.label}</div>
          {dir && (
            <div
              className="truncate text-xs"
              style={{ color: "var(--th-fg-muted)" }}
            >
              {dir}
            </div>
          )}
        </div>
        {when && (
          <span
            className="shrink-0 self-start pt-0.5 text-[10px] tabular-nums"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {when}
          </span>
        )}
      </button>
    </li>
  );
}
