// The sidebar's "Recent" list — a session library, GROUPED BY FOLDER.
//
// Recent = past Claude Code sessions the user can RESUME. The backend
// (`recent_sessions` -> recent.rs) returns a flat, newest-first list of sessions,
// each with its directory (`cwd`), a human description (`label` = Claude's
// summary, else the session's first real prompt, else the folder name), and a
// last-seen time.
//
// The user's model (2026-06-14): don't show the same folder over and over. Show
// each FOLDER once; expand it to scroll that folder's sessions. Resuming is an
// EXPLICIT button on a session row (not an accidental row click): it spawns a
// terminal in the session's cwd running `claude --resume <id>` (App wires
// `onRecall` -> the workspace store's `recall`).
//
// Fetched once on mount and on window focus (cheap: the backend reads only a
// 32KB prefix per transcript). Best-effort: an IPC failure degrades to a muted
// empty state, never an error surface.
import { useCallback, useEffect, useMemo, useState } from "react";
import { recentSessions, type RecentSession } from "../ipc/recent";

export interface RecentListProps {
  /** Resume a past session: spawn `claude --resume <id>` in `cwd`, focus it. */
  onRecall: (sessionId: string, cwd: string) => void;
}

/** Final path segment of a cwd (POSIX or Windows separators), or the whole
 *  string if it has none — the folder name shown on a group header. */
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

/** One folder's sessions, in newest-first order (the order the backend returned
 *  them, preserved). `newest` drives folder ordering + the header's time. */
interface FolderGroup {
  cwd: string;
  name: string;
  sessions: RecentSession[];
  newest: number;
}

/** Group a flat, newest-first session list by `cwd`, preserving newest-first
 *  order for both the folders (by their most-recent session) and the sessions
 *  within each folder. */
function groupByFolder(sessions: RecentSession[]): FolderGroup[] {
  const byCwd = new Map<string, FolderGroup>();
  for (const s of sessions) {
    let g = byCwd.get(s.cwd);
    if (!g) {
      g = { cwd: s.cwd, name: cwdBasename(s.cwd), sessions: [], newest: s.lastSeen };
      byCwd.set(s.cwd, g);
    }
    g.sessions.push(s);
    if (s.lastSeen > g.newest) g.newest = s.lastSeen;
  }
  // Map insertion order already follows newest-first (the input is sorted), but
  // sort defensively so a folder always sits at its most-recent session.
  return [...byCwd.values()].sort((a, b) => b.newest - a.newest);
}

export function RecentList({ onRecall }: RecentListProps) {
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [loaded, setLoaded] = useState(false);
  // Which folders are expanded (collapsed by default — the user wants to see a
  // clean list of folders, then expand the one they want to scroll).
  const [open, setOpen] = useState<Record<string, boolean>>({});

  const refresh = useCallback(() => {
    // No visibility guard: the fetch is cheap and an early return before
    // setLoaded(true) could stick the list on "Loading..." (see prior bug).
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

  const groups = useMemo(() => groupByFolder(sessions), [sessions]);

  const toggle = useCallback(
    (cwd: string) => setOpen((o) => ({ ...o, [cwd]: !o[cwd] })),
    [],
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

  return (
    <div className="flex flex-col gap-1 px-2 py-1">
      {groups.map((g) => (
        <FolderRow
          key={g.cwd}
          group={g}
          open={!!open[g.cwd]}
          onToggle={() => toggle(g.cwd)}
          onRecall={onRecall}
        />
      ))}
    </div>
  );
}

/** A collapsible folder: header (name + session count + newest time) over its
 *  sessions when expanded. Rounded card styling for the bigger, softer sidebar. */
function FolderRow({
  group,
  open,
  onToggle,
  onRecall,
}: {
  group: FolderGroup;
  open: boolean;
  onToggle: () => void;
  onRecall: (sessionId: string, cwd: string) => void;
}) {
  return (
    <div
      className="overflow-hidden rounded-lg"
      style={{ background: open ? "var(--th-tile-bg)" : "transparent" }}
    >
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left transition-colors hover:bg-neutral-800/40"
        style={{ color: "var(--th-fg)" }}
        title={group.cwd}
      >
        <span
          className="w-3 shrink-0 text-[11px] transition-transform"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {open ? "▾" : "▸"}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[13px] font-medium">{group.name}</span>
          <span
            className="block truncate text-[11px]"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {group.cwd}
          </span>
        </span>
        {/* Session-count pill so a busy folder reads as "many sessions" at a glance. */}
        <span
          className="shrink-0 rounded-full px-2 py-0.5 text-[10px] tabular-nums"
          style={{ background: "var(--th-header-bg)", color: "var(--th-fg-muted)" }}
          title={`${group.sessions.length} session${group.sessions.length === 1 ? "" : "s"}`}
        >
          {group.sessions.length}
        </span>
        <span
          className="shrink-0 text-[10px] tabular-nums"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {relativeTime(group.newest)}
        </span>
      </button>

      {open && (
        <ul className="flex flex-col gap-0.5 px-1.5 pb-1.5">
          {group.sessions.map((s) => (
            <SessionRow key={s.id} session={s} onRecall={onRecall} />
          ))}
        </ul>
      )}
    </div>
  );
}

/** One session under a folder: its description + last-seen time, with an EXPLICIT
 *  "Resume" button (the deliberate affordance the user asked for, so a stray row
 *  click can't launch a session). */
function SessionRow({
  session,
  onRecall,
}: {
  session: RecentSession;
  onRecall: (sessionId: string, cwd: string) => void;
}) {
  const when = relativeTime(session.lastSeen);
  return (
    <li
      className="group flex items-center gap-2 rounded-md px-2 py-1.5 transition-colors hover:bg-neutral-800/50"
      style={{ color: "var(--th-fg)" }}
    >
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[12.5px]" title={session.label}>
          {session.label}
        </span>
        {when && (
          <span className="text-[10px]" style={{ color: "var(--th-fg-muted)" }}>
            {when} ago
          </span>
        )}
      </span>
      <button
        type="button"
        onClick={() => onRecall(session.id, session.cwd)}
        className="shrink-0 rounded-md px-2.5 py-1 text-[11px] font-medium transition-colors"
        style={{
          background: "var(--th-accent)",
          color: "var(--th-accent-fg, #fff)",
        }}
        title={`Resume this session: claude --resume in ${session.cwd}`}
      >
        Resume
      </button>
    </li>
  );
}
