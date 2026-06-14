// Supervision store: holds the orchestrator→subagent trees and per-session
// statuses the core derives from the journal spine (Workstream C). Mirrors the
// workspace.ts zustand pattern. Fed by the supervision:// / session:// events
// (client05.onSupervision / onSessionStatus) and by an initial pull on mount.
//
// This is UI state only — the core's Supervisor is the source of truth; this
// caches the latest snapshot per session for rendering the tree view + queue.
import { create } from "zustand";
import type { SessionStatus, SupervisionTree } from "../ipc/model";

interface SupervisionState {
  /** Latest tree snapshot per orchestrator session id. */
  trees: Record<string, SupervisionTree>;
  /** Latest status per session id (also present on the tree, mirrored here for
   *  sessions that have a status but no subagents yet). */
  statuses: Record<string, SessionStatus>;

  /** Replace/insert a tree snapshot (from supervision:// event or a pull). */
  setTree: (tree: SupervisionTree) => void;
  /** Bulk-set trees (initial load). */
  setTrees: (trees: SupervisionTree[]) => void;
  /** Update a session's status (from session:// event). */
  setStatus: (sessionId: string, status: SessionStatus) => void;
  /** Drop a session (e.g. on SessionEnd cleanup). */
  remove: (sessionId: string) => void;
}

export const useSupervision = create<SupervisionState>((set) => ({
  trees: {},
  statuses: {},

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

  remove: (sessionId) =>
    set((s) => {
      const trees = { ...s.trees };
      const statuses = { ...s.statuses };
      delete trees[sessionId];
      delete statuses[sessionId];
      return { trees, statuses };
    }),
}));

/**
 * Derived selector: sessions that currently want the user's attention
 * (needsQuestion / needsPermission / failed / a freshly completed main turn),
 * for the sidebar attention queue (PLAN.md §F). Pure over the statuses map.
 */
export function attentionSessions(
  statuses: Record<string, SessionStatus>,
): { sessionId: string; status: SessionStatus }[] {
  const wants = new Set<SessionStatus>([
    "needsQuestion",
    "needsPermission",
    "failed",
    "completed",
  ]);
  return Object.entries(statuses)
    .filter(([, st]) => wants.has(st))
    .map(([sessionId, status]) => ({ sessionId, status }));
}
