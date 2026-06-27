// The sidebar's "Recent" list — a resumable session library, one row per PROJECT.
//
// Recent = past Claude Code sessions you can RESUME. The backend (`recent_sessions`
// -> recent.rs) returns a flat, newest-first list with ONE session per project (its
// most recent), each carrying its directory (`cwd`), a folder `name`/`label`, the
// session's most-recent message text (`lastText`), and a last-seen time.
//
// Per the 2026-06-15 redesign: NO Resume button, NO session dropdown. Each row is
// one PROJECT (folder), led by the Claude glyph (these ARE Claude sessions), and
// shows — so you can RECOGNIZE and recall it — the NAME you gave the work (the
// per-project work name, joined in frontend-side from the theme store; falls back
// to the folder name), the WORKTREE/branch context (derived here from the cwd: a
// `wt-<branch>` worktree segment, else the parent project folder), and the latest
// activity text. The worktree + name are joined on the FRONTEND so the backend
// `RecentSession` contract is untouched. On row hover, two understated controls
// appear on the RIGHT: a → that RESUMES (spawns a terminal in the cwd running
// `claude --resume <id>`, via onRecall) and an × that REMOVES the row from Recent
// for good. The × optimistically hides the row (persisted locally as a fallback),
// then ARCHIVES the project's transcripts out of `~/.claude/projects` into a
// sibling `projects-archive` dir (archiveRecentProject -> archive_recent_project in
// recent.rs) — so the project stops resurfacing AND stops costing scan time, while
// staying reversible (nothing is deleted). Fetched on mount + window focus; an IPC
// failure degrades to a muted empty state.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ClaudeIcon } from "./ClaudeIcon";
import {
  recentSessions,
  archiveRecentProject,
  type RecentSession,
} from "../ipc/recent";
import { useTheme } from "../store/theme";
import { useWorkspace } from "../store/workspace";
import { runWhenIdle } from "../lib/windowInteraction";

export interface RecentListProps {
  /** Resume a past session: spawn `claude --resume <id>` in `cwd`, focus it. */
  onRecall: (sessionId: string, cwd: string) => void;
  /** Report the visible (post hidden-filter) project count up to the sidebar so
   *  the "Recent" section header can show it. Called when the count changes. */
  onCount?: (n: number) => void;
}

/** Final path segment of a cwd (POSIX or Windows separators), or the whole string
 *  if it has none — the folder name shown on a row. */
function cwdBasename(cwd: string): string {
  const parts = cwd.replace(/[/\\]+$/, "").split(/[/\\]+/);
  return parts[parts.length - 1] || cwd;
}

/** Derive the session's WORKTREE / git context from its cwd — purely from the path,
 *  so no backend change is needed. We surface the worktree DIRECTORY the session
 *  ran inside, which in this workflow maps 1:1 to the branch it was on:
 *    - a sibling worktree folder named `wt-<branch>` (the project's convention) is
 *      the strongest signal — we return its branch part (`wt-terminal-input` →
 *      `terminal-input`); else
 *    - the repo/worktree root: the nearest ancestor segment that sits ABOVE the
 *      cwd basename when the session ran in a subdirectory (e.g. `apps/desktop` →
 *      `desktop`'s parent project folder), so the row still shows where it lived.
 *  Returns "" when the path is too shallow to add context beyond the folder name. */
function cwdWorktree(cwd: string): string {
  const parts = cwd
    .replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean);
  // Prefer an explicit `wt-*` worktree segment (the convention in this repo).
  for (let i = parts.length - 1; i >= 0; i -= 1) {
    const seg = parts[i];
    if (/^wt-/.test(seg)) return seg.replace(/^wt-/, "");
  }
  // Otherwise, when the session ran in a SUBDIR, surface the parent project folder
  // so the row still shows the worktree/repo it lived in (not just the leaf dir).
  if (parts.length >= 2) return parts[parts.length - 2];
  return "";
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

// --- Hidden rows: a persisted set of dismissed project CWDs (the × button). Keyed
// by cwd (NOT session id) so a dismissed project stays gone — it does NOT resurface
// when a newer session appears in it (which a session-id key would miss). v2 because
// the key meaning changed from id -> cwd.
const HIDDEN_KEY = "th.recent.hidden.v2";
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

// --- Cached list: the last sessions we fetched, so a reopen / window-focus
// renders the previous Recent INSTANTLY (no "Loading…" flash) while a fresh
// fetch runs in the background; we only re-render if the result actually changed
// (stale-while-revalidate).
const CACHE_KEY = "th.recent.cache.v1";
function loadCache(): RecentSession[] {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    return raw ? (JSON.parse(raw) as RecentSession[]) : [];
  } catch {
    return [];
  }
}
function saveCache(list: RecentSession[]): void {
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(list));
  } catch {
    /* localStorage unavailable — Recent just refetches next time */
  }
}

/** One project's row: its most-recent session plus the folder display name and the
 *  worktree/git context (both derived from the cwd — no backend change needed). */
interface FolderGroup {
  cwd: string;
  name: string;
  /** Worktree/branch context derived from the cwd (e.g. `terminal-input`), or "". */
  worktree: string;
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
    out.push({
      cwd: s.cwd,
      name: cwdBasename(s.cwd),
      worktree: cwdWorktree(s.cwd),
      session: s,
    });
  }
  return out;
}

/**
 * Cheap structural equality for two recent-session lists — the Option B
 * replacement for a per-refresh `JSON.stringify` deep-compare. RecentSession is a
 * flat record, so comparing every field by index covers exactly what stringify
 * did, but allocates nothing and short-circuits on the first difference.
 */
function sameRecent(a: RecentSession[], b: RecentSession[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.id !== y.id ||
      x.lastSeen !== y.lastSeen ||
      x.label !== y.label ||
      x.cwd !== y.cwd ||
      x.lastText !== y.lastText
    ) {
      return false;
    }
  }
  return true;
}

export function RecentList({ onRecall, onCount }: RecentListProps) {
  // Seed from cache so we render the previous Recent immediately; `loaded` is
  // true when a cache existed, so "Loading…" only shows on the very first run.
  const [sessions, setSessions] = useState<RecentSession[]>(loadCache);
  const [loaded, setLoaded] = useState(() => loadCache().length > 0);
  const [hidden, setHidden] = useState<Set<string>>(() => loadHidden());

  const refresh = useCallback(() => {
    void recentSessions()
      .then((list) => {
        setLoaded(true);
        // Only swap state when the list actually changed — an unchanged refetch
        // keeps the same array ref, so React skips the re-render (no flash).
        // Option B: a field-by-field compare (RecentSession is a flat 5-field
        // record) instead of JSON.stringify(prev)===JSON.stringify(list), which
        // serialized the whole list on EVERY focus/refresh on the main thread.
        // This covers the same fields but allocates nothing and short-circuits.
        setSessions((prev) => (sameRecent(prev, list) ? prev : list));
        saveCache(list);
      })
      .catch(() => setLoaded(true));
  }, []);

  useEffect(() => {
    refresh();
    // Defer the focus refresh past an active window drag (cold-first-drag fix).
    const onFocus = () => runWhenIdle(refresh);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  // The × button: DISMISS a project from Recent for good. We optimistically hide
  // the row instantly (also the graceful fallback if the archive fails — it stays
  // hidden locally exactly as the old behavior did), then durably archive the
  // project's transcripts out of the scanned catalog so it stops resurfacing AND
  // stops costing scan time, and refresh from the backend's new reality.
  const hide = useCallback(
    (cwd: string) => {
      setHidden((prev) => {
        const next = new Set(prev);
        next.add(cwd);
        saveHidden(next);
        return next;
      });
      void archiveRecentProject(cwd)
        .then(() => refresh())
        .catch(() => {
          /* archive move failed (e.g. daemon offline) — the optimistic hide
             keeps the row out of view; it may resurface on a later fresh scan. */
        });
    },
    [refresh],
  );

  // The cwds currently OPEN as tiles in this window. Recent is a library of work you
  // can resume — so it should show only projects you DON'T already have up; a project
  // open in a workspace is filtered out (and reappears once you close that tile).
  const terminals = useWorkspace((s) => s.terminals);
  const openCwds = useMemo(() => {
    const set = new Set<string>();
    for (const t of Object.values(terminals)) if (t.cwd) set.add(t.cwd);
    return set;
  }, [terminals]);

  const groups = useMemo(
    () =>
      groupByFolder(sessions).filter(
        (g) => !hidden.has(g.cwd) && !openCwds.has(g.cwd),
      ),
    [sessions, hidden, openCwds],
  );

  // Report the visible count up to the "Recent" section header (item 5).
  useEffect(() => {
    onCount?.(groups.length);
  }, [groups.length, onCount]);

  // Cosmetic per-project work names (keyed by cwd) — surfaced as the row title.
  const workNames = useTheme((s) => s.workNames);
  // Tint a Recent row with the color of the workspace that currently has a
  // terminal open in that project's cwd (best-effort: only currently-open,
  // colored workspaces tint; past-only projects use the default).
  const tabs = useWorkspace((s) => s.tabs);
  const workspaceColors = useTheme((s) => s.workspaceColors);
  const cwdColor = useMemo(() => {
    const m: Record<string, string> = {};
    for (const t of tabs) {
      const color = workspaceColors[t.id];
      if (!color) continue;
      for (const id of t.order) {
        const c = terminals[id]?.cwd;
        if (c && !(c in m)) m[c] = color;
      }
    }
    return m;
  }, [tabs, terminals, workspaceColors]);

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

  // Flat list of projects. The parent "Recent" Section caps the height and owns
  // the scroll for this region, so here we just stack the rows.
  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {groups.map((g) => (
        <ProjectRow
          key={g.cwd}
          group={g}
          workName={workNames[g.cwd]}
          color={cwdColor[g.cwd]}
          onRecall={onRecall}
          onHide={hide}
        />
      ))}
    </div>
  );
}

/** One project row: a Claude glyph, then the work name (or folder) over a subtitle
 *  carrying the worktree/branch context + the session's latest activity text. On
 *  hover, a → to RESUME and an × to HIDE appear on the right (both understated). */
function ProjectRow({
  group,
  workName,
  color,
  onRecall,
  onHide,
}: {
  group: FolderGroup;
  /** The user's cosmetic "work name" for this project (keyed by cwd), if set —
   *  shown as the row title in place of the bare folder name. */
  workName?: string;
  /** The owning workspace's color, when this project is open in a colored
   *  workspace — tints the row's left bar. */
  color?: string;
  onRecall: (sessionId: string, cwd: string) => void;
  onHide: (sessionId: string) => void;
}) {
  const s = group.session;
  // Busy gate (#7): a resume spawns a tmux session + `claude --resume`, which
  // takes a moment. Disable the → as soon as it's clicked so a double-click can't
  // fire a second recall (the store's recall also guards itself by sessionId; this
  // is the visible UI half). Reset shortly after so a deliberate later resume of a
  // project that didn't end up open still works.
  const [resuming, setResuming] = useState(false);
  // Hold the re-enable timer so it can be cleared on unmount — a successful resume
  // usually unmounts this row (hide-already-open) within the 1.5s, and an uncleared
  // timer would fire setResuming on an unmounted component.
  const resumeTimer = useRef<number | null>(null);
  useEffect(() => () => {
    if (resumeTimer.current != null) window.clearTimeout(resumeTimer.current);
  }, []);
  const resume = useCallback(() => {
    if (resuming) return;
    setResuming(true);
    onRecall(s.id, s.cwd);
    // The store guard owns correctness; this just re-enables the trigger after the
    // spawn has had time to settle (onRecall is fire-and-forget / returns void).
    if (resumeTimer.current != null) window.clearTimeout(resumeTimer.current);
    resumeTimer.current = window.setTimeout(() => setResuming(false), 1500);
  }, [resuming, onRecall, s.id, s.cwd]);
  // Item 5: surface WHEN this project was last in session (not the last-request
  // text). `relativeTime` is compact ("3h"); render it as a clear "… ago" label,
  // with the absolute timestamp on hover.
  const rel = relativeTime(s.lastSeen);
  const lastActive = rel ? (rel === "now" ? "active now" : `${rel} ago`) : "";
  const lastSeenAbs = s.lastSeen
    ? new Date(s.lastSeen * 1000).toLocaleString()
    : "";
  // The named work wins the title; the folder name then moves into the subtitle so
  // it's still visible.
  const title = workName || group.name;
  // Worktree/branch context (from the cwd). Only show it when it adds signal beyond
  // what the title already says — i.e. it isn't the same as the folder name shown.
  const worktree =
    group.worktree && group.worktree !== group.name ? group.worktree : "";

  return (
    <div
      className="group flex items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-neutral-800/25"
      style={{
        color: "var(--th-fg)",
        ...(color ? { boxShadow: `inset 2px 0 0 0 ${color}` } : {}),
      }}
      title={group.cwd}
    >
      {/* Claude glyph: marks each row as a recallable Claude session. Tinted with
          Claude's brand clay; sized to sit beside the row title. */}
      <ClaudeIcon
        size={14}
        className="shrink-0 self-start mt-0.5"
        style={{ color: "#D97757" }}
      />

      {/* LEFT: work name (or folder) over the session's most-recent text. */}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{title}</div>
        <div
          className="truncate text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title={lastSeenAbs ? `Last active ${lastSeenAbs}` : undefined}
        >
          {workName ? `${group.name} · ` : ""}
          {worktree ? `⎇ ${worktree} · ` : ""}
          {lastActive || "—"}
        </div>
      </div>

      {/* RIGHT (revealed on row hover or keyboard focus): resume arrow, then hide ×. */}
      <button
        type="button"
        onClick={resume}
        disabled={resuming}
        className="shrink-0 rounded-md px-2 py-1.5 text-[15px] leading-none opacity-0 transition-opacity hover:bg-neutral-700/50 focus:opacity-100 group-hover:opacity-100 disabled:cursor-not-allowed disabled:opacity-50"
        style={{ color: "var(--th-fg-muted)" }}
        title={`Resume: claude --resume in ${group.cwd}`}
        aria-label="Resume session"
      >
        →
      </button>
      <button
        type="button"
        onClick={() => onHide(group.cwd)}
        className="shrink-0 rounded-md px-2 py-1.5 text-[15px] leading-none opacity-0 transition-opacity hover:bg-neutral-700/50 focus:opacity-100 group-hover:opacity-100"
        style={{ color: "var(--th-fg-muted)" }}
        title="Remove this project from Recent — archives its transcripts (reversible)"
        aria-label="Remove from Recent"
      >
        ×
      </button>
    </div>
  );
}
