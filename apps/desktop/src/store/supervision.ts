// Supervision store: holds the orchestrator→subagent trees and per-session
// statuses the core derives from the journal spine (Workstream C). Mirrors the
// workspace.ts zustand pattern. Fed by the supervision:// / session:// events
// (client05.onSupervision / onSessionStatus) and by an initial pull on mount.
//
// This is UI state only — the core's Supervisor is the source of truth; this
// caches the latest snapshot per session for rendering the tree view + queue.
import { create } from "zustand";
import type { SessionStatus, StatusSnapshot, SupervisionTree } from "../ipc/model";
import { tlog } from "../lib/diag";

/**
 * Usage % at/above which a session is surfaced as `rateLimited` in the UI. The
 * statusline drives this (FR-012 `rateLimited` is NOT a reducer state — it is an
 * overlay derived from `rate_limits.*.used_percentage`). Conservative so we only
 * shout when a turn is genuinely at risk of being blocked.
 */
export const RATE_LIMIT_THRESHOLD = 90;

interface SupervisionState {
  /** Latest tree snapshot per orchestrator session id. */
  trees: Record<string, SupervisionTree>;
  /** Latest status per session id (also present on the tree, mirrored here for
   *  sessions that have a status but no subagents yet). */
  statuses: Record<string, SessionStatus>;
  /** Latest statusline snapshot per session id (context %, cost, rate limits). */
  snapshots: Record<string, StatusSnapshot>;
  /** Reverse index: tmux session name (`th_<id>`) -> the session id of the LATEST
   *  snapshot that reported it. Lets a tile/sidebar row resolve its session in
   *  O(1) (tmuxSessionMidTurn) instead of scanning every snapshot on each
   *  supervision event — it's read per tile AND per sidebar terminal row. */
  sessionIdByTmux: Record<string, string>;

  /** Replace/insert a tree snapshot (from supervision:// event or a pull). */
  setTree: (tree: SupervisionTree) => void;
  /** Bulk-set trees (initial load). */
  setTrees: (trees: SupervisionTree[]) => void;
  /** Update a session's status (from session:// event). */
  setStatus: (sessionId: string, status: SessionStatus) => void;
  /** Record a statusline snapshot (from status:// event). */
  setSnapshot: (snap: StatusSnapshot) => void;
  /** Drop a session (e.g. on SessionEnd cleanup). */
  remove: (sessionId: string) => void;
}

/**
 * Two snapshots are equivalent for the UI if every meaningful field matches — i.e.
 * everything EXCEPT `ingestedAtMs`, which ticks on every re-ingest even when nothing
 * actually changed. The statusline re-emits an identical snapshot many times/sec, so
 * without this the store updated + diag-logged on every one — a re-render + disk-IO
 * storm that froze the app (and the machine), and ballooned the diag log past 100 MB.
 */
function sameSnapshot(a: StatusSnapshot, b: StatusSnapshot): boolean {
  return (
    a.contextUsedPct === b.contextUsedPct &&
    a.costUsd === b.costUsd &&
    a.rateLimitsPresent === b.rateLimitsPresent &&
    a.cwd === b.cwd &&
    a.tmuxPane === b.tmuxPane &&
    a.tmuxSession === b.tmuxSession &&
    a.fiveHour?.usedPercentage === b.fiveHour?.usedPercentage &&
    a.fiveHour?.resetsAt === b.fiveHour?.resetsAt &&
    a.sevenDay?.usedPercentage === b.sevenDay?.usedPercentage &&
    a.sevenDay?.resetsAt === b.sevenDay?.resetsAt
  );
}

export const useSupervision = create<SupervisionState>((set, get) => ({
  trees: {},
  statuses: {},
  snapshots: {},
  sessionIdByTmux: {},

  setTree: (tree) =>
    set((s) => ({
      trees: { ...s.trees, [tree.sessionId]: tree },
      statuses: { ...s.statuses, [tree.sessionId]: tree.status },
    })),

  setTrees: (trees) =>
    set(() => {
      const treeMap: Record<string, SupervisionTree> = {};
      const statusMap: Record<string, SessionStatus> = {};
      for (const t of trees) {
        treeMap[t.sessionId] = t;
        statusMap[t.sessionId] = t.status;
      }
      return { trees: treeMap, statuses: statusMap };
    }),

  setStatus: (sessionId, status) =>
    set((s) => {
      // Keep the tree's own status field in sync if we have a tree.
      const tree = s.trees[sessionId];
      return {
        statuses: { ...s.statuses, [sessionId]: status },
        trees: tree
          ? { ...s.trees, [sessionId]: { ...tree, status } }
          : s.trees,
      };
    }),

  setSnapshot: (snap) => {
    // Drop NO-OP resends (the freeze fix). The statusline re-emits an identical
    // snapshot many times/sec — only `ingestedAtMs` ticks — so without this the
    // store updated + diag-logged on every one: a re-render + disk-IO storm. Bail
    // BEFORE set()/tlog() when nothing meaningful changed (no notify, no render).
    const prev = get().snapshots[snap.sessionId];
    if (prev && sameSnapshot(prev, snap)) return;
    // Real change: record it (and confirm the statusline->store chain in the diag).
    tlog(
      "usage",
      `setSnapshot ${snap.sessionId} ctx=${snap.contextUsedPct ?? "-"} cost=${snap.costUsd ?? "-"} rl5h=${snap.fiveHour?.usedPercentage ?? "-"}`,
    );
    set((s) => ({
      snapshots: { ...s.snapshots, [snap.sessionId]: snap },
      // Keep the tmux->session index current (latest snapshot wins). Only when
      // the snapshot carries its owning tmux session (un-upgraded agents omit it).
      sessionIdByTmux: snap.tmuxSession
        ? { ...s.sessionIdByTmux, [snap.tmuxSession]: snap.sessionId }
        : s.sessionIdByTmux,
    }));
  },

  remove: (sessionId) =>
    set((s) => {
      const trees = { ...s.trees };
      const statuses = { ...s.statuses };
      const snapshots = { ...s.snapshots };
      delete trees[sessionId];
      delete statuses[sessionId];
      delete snapshots[sessionId];
      // Drop any tmux->session index entries that pointed at the removed session.
      const sessionIdByTmux = { ...s.sessionIdByTmux };
      for (const [k, v] of Object.entries(sessionIdByTmux)) {
        if (v === sessionId) delete sessionIdByTmux[k];
      }
      return { trees, statuses, snapshots, sessionIdByTmux };
    }),
}));

/** The reducer statuses that mean a session is MID-TURN — killing its tile should
 *  confirm first. An idle / completed / failed / detached session kills now. */
const ACTIVE_TURN: ReadonlySet<SessionStatus> = new Set<SessionStatus>([
  "working",
  "needsQuestion",
  "needsPermission",
  "waitingOnSubagents",
]);

/**
 * True when the Claude session bound to `tmuxSession` (e.g. `th_<terminalId>`) is
 * MID-TURN, so killing its tile should ask first. Bridges the tile<->session gap
 * via the statusline snapshot, which now carries its owning tmux session: find the
 * snapshot whose `tmuxSession` matches, then read that session's reducer status.
 * Best-effort — false when nothing matches (no Claude here / un-upgraded agent /
 * the session is idle).
 */
export function tmuxSessionMidTurn(
  state: Pick<SupervisionState, "statuses" | "sessionIdByTmux">,
  tmuxSession: string,
): boolean {
  if (!tmuxSession) return false;
  const sessionId = state.sessionIdByTmux[tmuxSession];
  if (sessionId === undefined) return false;
  const st = state.statuses[sessionId];
  return st !== undefined && ACTIVE_TURN.has(st);
}

/**
 * The DISPLAY status of the agent session bound to `tmuxSession` (`th_<id>`), or
 * `undefined` when nothing is bound (a plain shell, or Claude before its first
 * statusline snapshot). Resolves the tile/row→session gap via `sessionIdByTmux`
 * (populated by the statusline snapshot), then applies the rate-limit overlay.
 * This is the precise per-terminal status the sidebar + tiles render — distinct
 * working / asking / idle states, instead of a raw output-activity pulse.
 */
export function sessionStatusForTmux(
  state: Pick<
    SupervisionState,
    "statuses" | "snapshots" | "sessionIdByTmux"
  >,
  tmuxSession: string,
): SessionStatus | undefined {
  if (!tmuxSession) return undefined;
  const sessionId = state.sessionIdByTmux[tmuxSession];
  if (sessionId === undefined) return undefined;
  const status = state.statuses[sessionId];
  if (status === undefined) return undefined;
  return displayStatus(status, state.snapshots[sessionId]);
}

/**
 * True when a statusline snapshot reports either rate-limit window at/over
 * {@link RATE_LIMIT_THRESHOLD}. False when the `rate_limits` block is absent
 * (free tier / pre-first-response — REVIEW caveat), so we never false-alarm.
 */
export function isRateLimited(snap: StatusSnapshot | undefined): boolean {
  if (!snap || !snap.rateLimitsPresent) return false;
  const over = (w?: { usedPercentage?: number }) =>
    (w?.usedPercentage ?? 0) >= RATE_LIMIT_THRESHOLD;
  return over(snap.fiveHour) || over(snap.sevenDay);
}

/**
 * The status to render for a session: the reducer status (FR-012 working /
 * waitingOnSubagents / needs* / completed / failed), overlaid with `rateLimited`
 * when the statusline says a window is near its cap AND the session is otherwise
 * mid-turn (working/waiting). A finished/failed turn keeps its terminal status —
 * a rate limit only matters while the agent still needs headroom to proceed.
 */
export function displayStatus(
  status: SessionStatus,
  snap: StatusSnapshot | undefined,
): SessionStatus {
  if (
    isRateLimited(snap) &&
    (status === "working" || status === "waitingOnSubagents")
  ) {
    return "rateLimited";
  }
  return status;
}

/**
 * Derived selector: sessions that currently want the user's attention
 * (needsQuestion / needsPermission / failed / rateLimited / a freshly completed
 * main turn), for the sidebar attention queue (PLAN.md §F). Pure over the
 * statuses map. Pass *display* statuses (rate-limit overlay already applied) so a
 * rate-limited session shows up here.
 */
export function attentionSessions(
  statuses: Record<string, SessionStatus>,
): { sessionId: string; status: SessionStatus }[] {
  const wants = new Set<SessionStatus>([
    "needsQuestion",
    "needsPermission",
    "failed",
    "rateLimited",
    "completed",
  ]);
  return Object.entries(statuses)
    .filter(([, st]) => wants.has(st))
    .map(([sessionId, status]) => ({ sessionId, status }));
}
