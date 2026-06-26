// Read-only orchestrator→subagent tree view (PLAN.md Workstream C — the
// headline 0.5 deliverable). Renders one orchestrator session with its live +
// finished subagent children and its outstanding-task count. Used in the
// sidebar/tile detail. Presentational + a thin subscribe to the supervision
// store; no direct IPC (the store is fed by client05 events elsewhere).
import type { SubagentNode, SupervisionTree as Tree } from "../ipc/model";
import { useSupervision } from "../store/supervision";
import { StatusBadge } from "./StatusBadge";
import { StatusIndicator } from "./StatusIndicator";

/** A single subagent row: a state dot, its type, and a duration when finished. */
function SubagentRow({ node }: { node: SubagentNode }) {
  const running = node.state === "running";
  const label = node.agentType ?? node.agentId.slice(0, 8);
  const duration =
    node.endedAt != null ? `${Math.max(0, node.endedAt - node.startedAt)}ms` : null;
  return (
    <li className="flex items-center gap-2 py-0.5 pl-4 text-xs">
      {/* Subagent state via the shared ring+center indicator: a running child
          pulses (working), a finished one is a calm solid (done). */}
      <StatusIndicator
        variant={running ? "working" : "done"}
        size={8}
        title={running ? "running" : "completed"}
      />
      <span className={running ? "text-neutral-200" : "text-neutral-400"}>{label}</span>
      {duration && <span className="text-neutral-600">{duration}</span>}
    </li>
  );
}

export interface SupervisionTreeViewProps {
  /** Render the tree for this orchestrator session id. */
  sessionId: string;
  /** Optional human label for the orchestrator (else the session id is shown). */
  label?: string;
}

/**
 * Renders the supervision tree for `sessionId` from the store. Shows nothing
 * (a muted hint) when no tree exists yet for the session.
 */
export function SupervisionTreeView({ sessionId, label }: SupervisionTreeViewProps) {
  const tree = useSupervision((s) => s.trees[sessionId]);
  if (!tree) {
    return (
      <div className="px-2 py-1 text-xs text-neutral-600">
        No subagent activity for this session yet.
      </div>
    );
  }
  return <SupervisionTreeBody tree={tree} label={label ?? sessionId} />;
}

/** The presentational body, also usable directly with a tree value. */
export function SupervisionTreeBody({ tree, label }: { tree: Tree; label: string }) {
  const running = tree.children.filter((c) => c.state === "running").length;
  const finished = tree.children.length - running;
  return (
    <div className="select-none px-2 py-1">
      {/* Orchestrator header: label + status + counts. */}
      <div className="flex items-center gap-2">
        <StatusBadge status={tree.status} />
        <span className="min-w-0 flex-1 truncate text-xs text-neutral-300" title={label}>
          {label}
        </span>
      </div>

      {/* Counts line. */}
      <div className="mt-0.5 flex items-center gap-3 pl-0 text-[11px] text-neutral-500">
        <span title="running subagents">{running} running</span>
        <span title="finished subagents">{finished} done</span>
        {tree.outstandingTasks > 0 && (
          <span className="text-amber-400" title="outstanding background tasks">
            {tree.outstandingTasks} task{tree.outstandingTasks === 1 ? "" : "s"}
          </span>
        )}
      </div>

      {/* Children (sorted by the core: started_at then agent_id). */}
      {tree.children.length > 0 && (
        <ul className="mt-1 border-l border-neutral-800">
          {tree.children.map((node) => (
            <SubagentRow key={node.agentId} node={node} />
          ))}
        </ul>
      )}
    </div>
  );
}
