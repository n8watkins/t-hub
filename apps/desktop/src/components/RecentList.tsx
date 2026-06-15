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

/** One project row: [Resume] · name + SELECTED session · [▾ sessions]. The ▾
 *  dropdown only SELECTS which session this row targets (it does NOT resume); the
 *  understated Resume button is what actually launches `claude --resume`. */
function ProjectRow({
  group,
  onRecall,
}: {
  group: FolderGroup;
  onRecall: (sessionId: string, cwd: string) => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  // Which session Resume will resume. Defaults to the project's most-recent;
  // the dropdown changes it. Keyed reset if the group's sessions change identity.
  const [selectedId, setSelectedId] = useState(group.sessions[0]?.id);
  const btnRef = useRef<HTMLButtonElement>(null);
  const hasMore = group.sessions.length > 1;

  const selected =
    group.sessions.find((s) => s.id === selectedId) ?? group.sessions[0];

  return (
    <div
      className="flex items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-neutral-800/40"
      style={{ color: "var(--th-fg)" }}
      title={group.cwd}
    >
      {/* LEFT: understated Resume — resumes the SELECTED session (only this
          launches Claude; picking in the dropdown does not). */}
      <button
        type="button"
        onClick={() => selected && onRecall(selected.id, selected.cwd)}
        className="shrink-0 rounded-md border px-2.5 py-1 text-[11px] font-medium transition-colors hover:bg-neutral-700/40"
        style={{
          background: "var(--th-tile-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg-muted)",
        }}
        title={`Resume the selected session: claude --resume in ${group.cwd}`}
      >
        Resume
      </button>

      {/* MIDDLE: project name over the SELECTED session's description. */}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{group.name}</div>
        <div
          className="truncate text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title={selected?.label}
        >
          {selected?.label}
          {selected && relativeTime(selected.lastSeen)
            ? ` · ${relativeTime(selected.lastSeen)}`
            : ""}
        </div>
      </div>

      {/* RIGHT: session dropdown — SELECT any of this project's sessions (does not
          resume). Only shown when there's more than one. */}
      {hasMore && (
        <button
          ref={btnRef}
          type="button"
          onClick={() => setMenuOpen((v) => !v)}
          className="shrink-0 rounded-md px-1.5 py-1 text-[11px] transition-colors hover:bg-neutral-700/40"
          style={{ color: "var(--th-fg-muted)" }}
          title={`${group.sessions.length} sessions — pick which one Resume targets`}
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
          selectedId={selected?.id}
          onPick={(s) => {
            setSelectedId(s.id); // SELECT only — Resume runs it
            setMenuOpen(false);
          }}
          onClose={() => setMenuOpen(false)}
        />
      )}
    </div>
  );
}

/** Fixed-position popup to SELECT one of a project's sessions (newest first),
 *  anchored under the ▾ button. Fixed (not inline) so the narrow/scrollable
 *  sidebar can't clip it. OPAQUE (composited over a solid base so a translucent
 *  theme surface doesn't show the app through it) and compact + scrollable
 *  (capped height) so a project with many sessions doesn't fill the screen. */
function SessionMenu({
  anchor,
  sessions,
  selectedId,
  onPick,
  onClose,
}: {
  anchor: HTMLElement | null;
  sessions: RecentSession[];
  selectedId?: string;
  onPick: (s: RecentSession) => void;
  onClose: () => void;
}) {
  const [pos, setPos] = useState<{ left: number; top: number; width: number } | null>(
    null,
  );

  useLayoutEffect(() => {
    if (!anchor) return;
    const r = anchor.getBoundingClientRect();
    const width = 280;
    const left = Math.max(8, Math.min(r.right - width, window.innerWidth - width - 8));
    setPos({ left, top: r.bottom + 4, width });
  }, [anchor]);

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
        // Compact + scrollable: ~9 rows tall then scrolls (not a giant dropdown).
        className="th-scroll fixed z-50 max-h-72 overflow-y-auto rounded-lg border py-1 shadow-2xl"
        style={{
          left: pos.left,
          top: pos.top,
          width: pos.width,
          // OPAQUE: layer the (possibly translucent) themed surface over a solid
          // dark base so nothing behind the menu shows through.
          background:
            "linear-gradient(var(--th-header-bg), var(--th-header-bg)), #0b0b0c",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
        onPointerDown={(e) => e.stopPropagation()}
      >
        {sessions.map((s) => {
          const active = s.id === selectedId;
          return (
            <button
              key={s.id}
              type="button"
              role="menuitemradio"
              aria-checked={active}
              onClick={() => onPick(s)}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-neutral-700/40"
              style={
                active
                  ? {
                      background:
                        "color-mix(in srgb, var(--th-accent) 16%, transparent)",
                    }
                  : undefined
              }
            >
              {/* Selection check so it's clear which session Resume targets. */}
              <span
                className="w-3 shrink-0 text-[11px]"
                style={{ color: "var(--th-accent)" }}
                aria-hidden
              >
                {active ? "✓" : ""}
              </span>
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
          );
        })}
      </div>
    </>
  );
}
