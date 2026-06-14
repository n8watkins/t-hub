// useAgentTelemetry: a React hook that wires the 0.5 read-only awareness UI to
// the core. On mount it:
//   - subscribes to supervision:// tree snapshots and session:// status changes
//     (feeding the supervision store),
//   - subscribes to agent://state (connection state),
//   - pulls the initial supervision trees (one per known session id), and
//   - polls host metrics on an interval for the WSL health display.
//
// All wiring degrades gracefully: before the agent bridge transport is
// connected, metrics/tree pulls simply reject and are ignored, and the UI shows
// "pending"/"offline" states. Lives in src/store so components stay pure.
import { useEffect, useState } from "react";
import {
  agentState,
  hostMetrics,
  onAgentState,
  onSessionStatus,
  onStatus,
  onSupervision,
  statusSnapshot,
  supervisionSessionIds,
  supervisionTree,
} from "../ipc/client05";
import type { AgentStateInfo, HostMetrics } from "../ipc/protocol";
import { useSupervision } from "./supervision";

/** How often to poll WSL host metrics (ms). Low-priority chrome. */
const METRICS_POLL_MS = 4000;

export interface AgentTelemetry {
  metrics: HostMetrics | null;
  agent: AgentStateInfo | null;
}

/**
 * Subscribe-and-poll the 0.5 telemetry surface. Returns the latest host metrics
 * + connection state for the utility area; the supervision store is updated as
 * a side effect (components read it directly via useSupervision).
 */
export function useAgentTelemetry(): AgentTelemetry {
  const [metrics, setMetrics] = useState<HostMetrics | null>(null);
  const [agent, setAgent] = useState<AgentStateInfo | null>(null);
  const setTree = useSupervision((s) => s.setTree);
  const setTrees = useSupervision((s) => s.setTrees);
  const setStatus = useSupervision((s) => s.setStatus);
  const setSnapshot = useSupervision((s) => s.setSnapshot);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    // Event subscriptions (live updates from the journal spine).
    void onSupervision((tree) => {
      if (!disposed) setTree(tree);
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});

    void onSessionStatus((e) => {
      if (!disposed) setStatus(e.sessionId, e.status);
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});

    void onAgentState((s) => {
      if (!disposed) setAgent(s);
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});

    void onStatus((snap) => {
      if (!disposed) setSnapshot(snap);
    })
      .then((fn) => (disposed ? fn() : unlisteners.push(fn)))
      .catch(() => {});

    // Initial pulls: connection state + a one-shot supervision snapshot.
    void agentState()
      .then((s) => !disposed && setAgent(s))
      .catch(() => {});

    void supervisionSessionIds()
      .then(async (ids) => {
        const [trees, snaps] = await Promise.all([
          Promise.all(ids.map((id) => supervisionTree(id).catch(() => null))),
          Promise.all(ids.map((id) => statusSnapshot(id).catch(() => null))),
        ]);
        if (!disposed) {
          setTrees(trees.filter((t): t is NonNullable<typeof t> => t != null));
          for (const snap of snaps) if (snap) setSnapshot(snap);
        }
      })
      .catch(() => {});

    // Metrics poll. Rejects (agent offline) are swallowed; the UI shows pending.
    const poll = () => {
      void hostMetrics()
        .then((m) => !disposed && setMetrics(m))
        .catch(() => {});
    };
    poll();
    const timer = setInterval(poll, METRICS_POLL_MS);

    return () => {
      disposed = true;
      clearInterval(timer);
      for (const fn of unlisteners) fn();
    };
  }, [setTree, setTrees, setStatus, setSnapshot]);

  return { metrics, agent };
}
