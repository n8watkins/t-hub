// The sidebar's "Recent" list — a resumable session library, one row per PROJECT.
//
// Recent = past Claude Code sessions the user can RESUME. The backend
// (`recent_sessions` -> recent.rs) returns a flat, newest-first list of sessions,
// each with its directory (`cwd`), a human description (`label` = Claude's
// summary, else the session's first real prompt, else the folder name), and a
// last-seen time.
//
// The user's model (2026-06-14): NO accordion. Show each PROJECT (folder) once,
// in a flat SCROLLABLE list (there can be hundreds). Each row shows the project's
// most-recent session, with an explicit (deliberately understated) "Resume"
// button on the LEFT and a session DROPDOWN on the RIGHT to pick any of that
// project's other sessions. Resuming spawns a terminal in the session's cwd
// running `claude --resume <id>` (App wires `onRecall` -> the store's `recall`).
//
// Fetched once on mount + on window focus (cheap: the backend stats every file
// but only reads a 32KB prefix of the newest N). Best-effort: an IPC failure
// degrades to a muted empty state.
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { recentSessions, type RecentSession } from "../ipc/recent";

export interface RecentListProps {
  /** Resume a past session: spawn `claude --resume <id>` in `cwd`, focus it. */
  onRecall: (sessionId: string, cwd: string) => void;
}

/** Final path segment of a cwd (POSIX or Windows separators), or the whole
 *  string if it has none — the folder name shown on a row. */
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

/** One project's sessions, newest-first. `newest` drives row ordering + the
 *  header time; `sessions[0]` is what Resume targets by default. */
interface FolderGroup {
  cwd: string;
  name: string;
  sessions: RecentSession[];
  newest: number;
}

/** Group a flat, newest-first session list by `cwd`, preserving newest-first
 *  order for both the projects (by most-recent session) and within each. */
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
  return [...byCwd.values()].sort((a, b) => b.newest - a.newest);
}

export function RecentList({ onRecall }: RecentListProps) {
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [loaded, setLoaded] = useState(false);

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

  const groups = useMemo(() => groupByFolder(sessions), [sessions]);

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

  // Flat, scrollable list of projects (no accordion). The parent section already
  // scrolls; this just stacks rows.
  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {groups.map((g) => (
        <ProjectRow key={g.cwd} group={g} onRecall={onRecall} />
      ))}
    </div>
  );
}

/** One project row: [Resume] · name + latest session · [▾ sessions]. The Resume
 *  button is intentionally understated (not the bright accent). The ▾ opens a
 *  fixed-position popup listing every session of this project to resume. */
function ProjectRow({
  group,
  onRecall,
}: {
  group: FolderGroup;
  onRecall: (sessionId: string, cwd: string) => void;
}) {
  const latest = group.sessions[0];
  const [menuOpen, setMenuOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const hasMore = group.sessions.length > 1;

  return (
    <div
      className="flex items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-neutral-800/40"
      style={{ color: "var(--th-fg)" }}
      title={group.cwd}
    >
      {/* LEFT: understated Resume (resumes the project's most-recent session). */}
      <button
        type="button"
        onClick={() => onRecall(latest.id, latest.cwd)}
        className="shrink-0 rounded-md border px-2.5 py-1 text-[11px] font-medium transition-colors hover:bg-neutral-700/40"
        style={{
          // Darker / subtle (per feedback): tile surface + border, NOT the bright
          // accent — a quiet affordance, not a call-to-action.
          background: "var(--th-tile-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg-muted)",
        }}
        title={`Resume the latest session: claude --resume in ${group.cwd}`}
      >
        Resume
      </button>

      {/* MIDDLE: project name over the latest session's description. */}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{group.name}</div>
        <div
          className="truncate text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title={latest.label}
        >
          {latest.label}
          {relativeTime(latest.lastSeen) && ` · ${relativeTime(latest.lastSeen)}`}
        </div>
      </div>

      {/* RIGHT: session dropdown — pick any of this project's sessions. Only
          shown when there's more than one (otherwise Resume already covers it). */}
      {hasMore && (
        <button
          ref={btnRef}
          type="button"
          onClick={() => setMenuOpen((v) => !v)}
          className="shrink-0 rounded-md px-1.5 py-1 text-[11px] transition-colors hover:bg-neutral-700/40"
          style={{ color: "var(--th-fg-muted)" }}
          title={`${group.sessions.length} sessions — pick one to resume`}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
        >
          {group.sessions.length}&nbsp;▾
        </button>
      )}

      {menuOpen && (
        <SessionMenu
          anchor={btnRef.current}
          sessions={group.sessions}
          onPick={(s) => {
            setMenuOpen(false);
            onRecall(s.id, s.cwd);
          }}
          onClose={() => setMenuOpen(false)}
        />
      )}
    </div>
  );
}

/** Fixed-position popup of a project's sessions (newest first), anchored under
 *  the ▾ button. Fixed (not inline) so the scrollable/narrow sidebar can't clip
 *  it; scrolls internally when a project has many sessions. */
function SessionMenu({
  anchor,
  sessions,
  onPick,
  onClose,
}: {
  anchor: HTMLElement | null;
  sessions: RecentSession[];
  onPick: (s: RecentSession) => void;
  onClose: () => void;
}) {
  const [pos, setPos] = useState<{ left: number; top: number; width: number } | null>(
    null,
  );

  // Anchor the menu to the button's on-screen rect, opening to the LEFT (the
  // button sits at the sidebar's right edge) and below it.
  useLayoutEffect(() => {
    if (!anchor) return;
    const r = anchor.getBoundingClientRect();
    const width = 280;
    const left = Math.max(8, Math.min(r.right - width, window.innerWidth - width - 8));
    setPos({ left, top: r.bottom + 4, width });
  }, [anchor]);

  // Close on Esc.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  if (!pos) return null;

  return (
    <>
      {/* Click-away backdrop. */}
      <div className="fixed inset-0 z-40" onPointerDown={onClose} aria-hidden />
      <div
        role="menu"
        className="fixed z-50 max-h-[60vh] overflow-y-auto rounded-lg border py-1 shadow-2xl"
        style={{
          left: pos.left,
          top: pos.top,
          width: pos.width,
          background: "var(--th-header-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
        onPointerDown={(e) => e.stopPropagation()}
      >
        {sessions.map((s) => (
          <button
            key={s.id}
            type="button"
            role="menuitem"
            onClick={() => onPick(s)}
            className="flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-neutral-700/40"
          >
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[12.5px]" title={s.label}>
                {s.label}
              </span>
            </span>
            <span
              className="shrink-0 text-[10px] tabular-nums"
              style={{ color: "var(--th-fg-muted)" }}
            >
              {relativeTime(s.lastSeen)}
            </span>
          </button>
        ))}
      </div>
    </>
  );
}
