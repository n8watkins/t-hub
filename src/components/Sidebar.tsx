// The 0.5 sidebar — the daily supervision surface (PLAN.md §F, read-only for
// 0.5). Three stacked areas:
//   1. Attention queue: sessions wanting input (question/permission/failure) or
//      a freshly completed main turn. Clicking a row calls onSelectSession.
//   2. Session/supervision tree: every supervised orchestrator with its
//      subagent children + outstanding task count (the headline 0.5 view).
//   3. Utility area: compact WSL health + agent connection state (low priority).
//
// Composes the presentational components + the supervision store + the
// telemetry hook. No direct IPC beyond the hook. Inert when there's no agent
// data yet (shows muted "no sessions"/"pending" states).
import { useSupervision, attentionSessions } from "../store/supervision";
import { useAgentTelemetry } from "../store/telemetry";
import { SupervisionTreeBody } from "./SupervisionTree";
import { StatusBadge, statusLabel } from "./StatusBadge";
import { WslHealth } from "./WslHealth";

export interface SidebarProps {
  /** Called when the user clicks an attention-queue row or a tree header. */
  onSelectSession?: (sessionId: string) => void;
}

export function Sidebar({ onSelectSession }: SidebarProps) {
  const { metrics, agent } = useAgentTelemetry();
  const trees = useSupervision((s) => s.trees);
  const statuses = useSupervision((s) => s.statuses);

  const treeList = Object.values(trees).sort((a, b) =>
    a.sessionId.localeCompare(b.sessionId),
  );
  const queue = attentionSessions(statuses);

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-neutral-800 bg-neutral-950 text-neutral-200">
      {/* 1. Attention queue */}
      <section className="border-b border-neutral-800">
        <Header>Attention</Header>
        {queue.length === 0 ? (
          <Muted>Nothing needs you.</Muted>
        ) : (
          <ul>
            {queue.map(({ sessionId, status }) => (
              <li key={sessionId}>
                <button
                  type="button"
                  onClick={() => onSelectSession?.(sessionId)}
                  className="flex w-full items-center gap-2 px-2 py-1 text-left text-xs hover:bg-neutral-900"
                  title={`${statusLabel(status)} — ${sessionId}`}
                >
                  <StatusBadge status={status} dotOnly />
                  <span className="min-w-0 flex-1 truncate text-neutral-300">
                    {sessionId}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* 2. Session / supervision tree (scrolls) */}
      <section className="min-h-0 flex-1 overflow-y-auto">
        <Header>Sessions</Header>
        {treeList.length === 0 ? (
          <Muted>No supervised sessions.</Muted>
        ) : (
          <div className="divide-y divide-neutral-900">
            {treeList.map((tree) => (
              <button
                key={tree.sessionId}
                type="button"
                onClick={() => onSelectSession?.(tree.sessionId)}
                className="block w-full text-left hover:bg-neutral-900/50"
              >
                <SupervisionTreeBody tree={tree} label={tree.sessionId} />
              </button>
            ))}
          </div>
        )}
      </section>

      {/* 3. Utility area (low priority) */}
      <section className="border-t border-neutral-800">
        <Header>WSL</Header>
        <WslHealth metrics={metrics} connection={agent?.connection} />
      </section>
    </aside>
  );
}

function Header({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-2 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-600">
      {children}
    </div>
  );
}

function Muted({ children }: { children: React.ReactNode }) {
  return <div className="px-2 py-1 text-xs text-neutral-600">{children}</div>;
}
