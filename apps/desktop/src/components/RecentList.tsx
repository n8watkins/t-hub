// The sidebar's "Recent" list — a resumable session library, one row per PROJECT.
//
// Recent = past Claude Code sessions you can RESUME. The backend (`recent_sessions`
// -> recent.rs) returns a flat, newest-first list with ONE session per project (its
// most recent), each carrying its directory (`cwd`), a folder `name`/`label`, the
// session's most-recent message text (`lastText`), and a last-seen time.
//
// Per the 2026-06-15 redesign: NO Resume button, NO session dropdown. Each row is
// one PROJECT (folder) showing the folder name over the latest activity text. On
// row hover, two understated controls appear on the RIGHT: a → that RESUMES (spawns
// a terminal in the cwd running `claude --resume <id>`, via onRecall) and an × that
// HIDES the row from Recent. Hiding is persisted locally and does NOT delete the
// transcript; a project resurfaces if it later gets a newer session. Fetched on
// mount + window focus; an IPC failure degrades to a muted empty state.
import { useCallback, useEffect, useMemo, useState } from "react";
import { recentSessions, type RecentSession } from "../ipc/recent";

export interface RecentListProps {
  /** Resume a past session: spawn `claude --resume <id>` in `cwd`, focus it. */
  onRecall: (sessionId: string, cwd: string) => void;
}

/** Final path segment of a cwd (POSIX or Windows separators), or the whole string
 *  if it has none — the folder name shown on a row. */
function cwdBasename(cwd: string): string {
  const parts = cwd.replace(/[/\\]+$/, "").split(/[/\\]+/);
  return parts[parts.length - 1] || cwd;
}

/** Compact relative time ("now", "3m", "2h", "5d", "3mo") from epoch SECONDS. */
function relativeTime(epochSecs: number): string {
  if (!epochSecs) return "";
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - epochSecs);
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 2592000) return `${Math.floor(diff / 86400)}d`;
  return `${Math.floor(diff / 2592000)}mo`;
}

// --- Hidden rows: a persisted set of dismissed session ids (the × button). Keyed
// by the row's most-recent session id, so dismissing hides THIS project now but it
// resurfaces if a newer session appears (its newest id changes -> no longer hidden).
const HIDDEN_KEY = "th.recent.hidden.v1";
function loadHidden(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_KEY);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}
function saveHidden(ids: Set<string>): void {
  try {
    localStorage.setItem(HIDDEN_KEY, JSON.stringify([...ids]));
  } catch {
    /* localStorage unavailable — dismissals just won't persist */
  }
}

/** One project's row: its most-recent session plus the folder display name. */
interface FolderGroup {
  cwd: string;
  name: string;
  session: RecentSession;
}

/** Reduce the flat, newest-first session list to one row per cwd (newest wins).
 *  The backend already caps to one session per project, but we dedupe defensively
 *  in case a cwd shows up twice; newest-first order is preserved. */
function groupByFolder(sessions: RecentSession[]): FolderGroup[] {
  const seen = new Set<string>();
  const out: FolderGroup[] = [];
  for (const s of sessions) {
    if (seen.has(s.cwd)) continue;
    seen.add(s.cwd);
    out.push({ cwd: s.cwd, name: cwdBasename(s.cwd), session: s });
  }
  return out;
}

export function RecentList({ onRecall }: RecentListProps) {
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [hidden, setHidden] = useState<Set<string>>(() => loadHidden());

  const refresh = useCallback(() => {
    void recentSessions()
      .then((list) => {
        setSessions(list);
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
  }, []);

  useEffect(() => {
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  const hide = useCallback((sessionId: string) => {
    setHidden((prev) => {
      const next = new Set(prev);
      next.add(sessionId);
      saveHidden(next);
      return next;
    });
  }, []);

  const groups = useMemo(
    () => groupByFolder(sessions).filter((g) => !hidden.has(g.session.id)),
    [sessions, hidden],
  );

  if (!loaded) {
    return (
      <div className="px-3 py-2 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        Loading…
      </div>
    );
  }
  if (groups.length === 0) {
    return (
      <div className="px-3 py-2 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        No recent Claude sessions to resume.
      </div>
    );
  }

  // Flat, scrollable list of projects. The parent section already scrolls; this
  // just stacks rows.
  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {groups.map((g) => (
        <ProjectRow key={g.cwd} group={g} onRecall={onRecall} onHide={hide} />
      ))}
    </div>
  );
}

/** One project row: folder name over the session's latest activity text. On hover,
 *  a → to RESUME and an × to HIDE appear on the right (both understated). */
function ProjectRow({
  group,
  onRecall,
  onHide,
}: {
  group: FolderGroup;
  onRecall: (sessionId: string, cwd: string) => void;
  onHide: (sessionId: string) => void;
}) {
  const s = group.session;
  // Prefer the session's most-recent text; fall back to its summary/first-prompt
  // label when the transcript tail yielded nothing usable.
  const subtitle = s.lastText || s.label;
  const rel = relativeTime(s.lastSeen);

  return (
    <div
      className="group flex items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-neutral-800/40"
      style={{ color: "var(--th-fg)" }}
      title={group.cwd}
    >
      {/* LEFT: folder name over the session's most-recent text. */}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{group.name}</div>
        <div
          className="truncate text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title={subtitle}
        >
          {subtitle}
          {rel ? ` · ${rel}` : ""}
        </div>
      </div>

      {/* RIGHT (revealed on row hover or keyboard focus): resume arrow, then hide ×. */}
      <button
        type="button"
        onClick={() => onRecall(s.id, s.cwd)}
        className="shrink-0 rounded-md px-1.5 py-1 text-[13px] leading-none opacity-0 transition-opacity hover:bg-neutral-700/40 focus:opacity-100 group-hover:opacity-100"
        style={{ color: "var(--th-fg-muted)" }}
        title={`Resume: claude --resume in ${group.cwd}`}
        aria-label="Resume session"
      >
        →
      </button>
      <button
        type="button"
        onClick={() => onHide(s.id)}
        className="shrink-0 rounded-md px-1.5 py-1 text-[13px] leading-none opacity-0 transition-opacity hover:bg-neutral-700/40 focus:opacity-100 group-hover:opacity-100"
        style={{ color: "var(--th-fg-muted)" }}
        title="Hide from Recent (does not delete the transcript)"
        aria-label="Hide from Recent"
      >
        ×
      </button>
    </div>
  );
}
