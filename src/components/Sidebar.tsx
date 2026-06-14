// The 0.5 sidebar — the daily supervision surface (PLAN.md §F, read-only for
// 0.5). Four stacked areas:
//   1. Attention queue: sessions wanting input (question/permission/failure),
//      rate-limited, or a freshly completed main turn. Clicking a row calls
//      onSelectSession.
//   2. Session/supervision tree: every supervised orchestrator with its
//      subagent children + outstanding task count (the headline 0.5 view), each
//      with a compact context-usage line from the statusline snapshot.
//   3. Hooks: the consent-gated Claude hook install/uninstall control.
//   4. Utility area: compact WSL health + agent connection state (low priority).
//
// Composes the presentational components + the supervision store + the
// telemetry hook. No direct IPC beyond the hook + the install panel. Inert when
// there's no agent data yet (muted "no sessions"/"pending" states). All live
// data arrives via the agent bridge's event emit spine (agent://journal →
// supervision://tree / session://status / status://snapshot); the telemetry hook
// subscribes and feeds the store.
import { useMemo } from "react";
import {
  useSupervision,
  attentionSessions,
  displayStatus,
} from "../store/supervision";
import { useAgentTelemetry } from "../store/telemetry";
import { SupervisionTreeBody } from "./SupervisionTree";
import { StatusBadge, statusLabel } from "./StatusBadge";
import { WslHealth } from "./WslHealth";
import { HookInstallPanel } from "./HookInstallPanel";
import type { StatusSnapshot, SupervisionTree } from "../ipc/model";

export interface SidebarProps {
  /** Called when the user clicks an attention-queue row or a tree header. */
  onSelectSession?: (sessionId: string) => void;
  /** Sidebar width in px (resizable, #2). Defaults to 256 (the old fixed w-64). */
  width?: number;
  /**
   * Resolved path to the termhub-agent binary used as the hook entrypoint
   * (`<agentBin> --hook <EVENT>`). Inside WSL `termhub-agent` is on PATH, so the
   * bare name is the right default; a dev box can override it.
   */
  agentBin?: string;
}

export function Sidebar({
  onSelectSession,
  width = 256,
  agentBin = "termhub-agent",
}: SidebarProps) {
  const { metrics, agent } = useAgentTelemetry();
  const trees = useSupervision((s) => s.trees);
  const statuses = useSupervision((s) => s.statuses);
  const snapshots = useSupervision((s) => s.snapshots);

  // Apply the rate-limit overlay (FR-012 `rateLimited` is a statusline overlay,
  // not a reducer state) to every known session's status before deriving the
  // queue and rendering badges, so a near-cap session reads as rate-limited.
  const displayStatuses = useMemo(() => {
    const out: Record<string, (typeof statuses)[string]> = {};
    for (const [sid, st] of Object.entries(statuses)) {
      out[sid] = displayStatus(st, snapshots[sid]);
    }
    return out;
  }, [statuses, snapshots]);

  const treeList = Object.values(trees).sort((a, b) =>
    a.sessionId.localeCompare(b.sessionId),
  );
  const queue = attentionSessions(displayStatuses);

  return (
    <aside
      className="flex h-full shrink-0 flex-col border-r"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
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
              <SessionRow
                key={tree.sessionId}
                tree={tree}
                displayStatus={displayStatuses[tree.sessionId] ?? tree.status}
                snapshot={snapshots[tree.sessionId]}
                onSelect={() => onSelectSession?.(tree.sessionId)}
              />
            ))}
          </div>
        )}
      </section>

      {/* 3. Claude hooks (consent-gated install/uninstall) */}
      <section className="border-t border-neutral-800">
        <HookInstallPanel agentBin={agentBin} />
      </section>

      {/* 4. Utility area (low priority) */}
      <section className="border-t border-neutral-800">
        <Header>WSL</Header>
        <WslHealth metrics={metrics} connection={agent?.connection} />
      </section>
    </aside>
  );
}

/** One orchestrator row: the supervision tree body (with the overlaid status)
 *  plus a compact statusline-usage line when a snapshot exists. */
function SessionRow({
  tree,
  displayStatus,
  snapshot,
  onSelect,
}: {
  tree: SupervisionTree;
  displayStatus: SupervisionTree["status"];
  snapshot?: StatusSnapshot;
  onSelect: () => void;
}) {
  // Render the tree with the rate-limit-overlaid status so the badge matches the
  // attention queue (the tree's own status field is the raw reducer status).
  const overlaid: SupervisionTree = { ...tree, status: displayStatus };
  return (
    <button
      type="button"
      onClick={onSelect}
      className="block w-full text-left hover:bg-neutral-900/50"
    >
      <SupervisionTreeBody tree={overlaid} label={tree.sessionId} />
      {snapshot && <UsageLine snapshot={snapshot} />}
    </button>
  );
}

/** A dense one-liner: context %, cost, and the nearest rate-limit window %. */
function UsageLine({ snapshot }: { snapshot: StatusSnapshot }) {
  const ctx = snapshot.contextUsedPct;
  const cost = snapshot.costUsd;
  const rl = Math.max(
    snapshot.fiveHour?.usedPercentage ?? 0,
    snapshot.sevenDay?.usedPercentage ?? 0,
  );
  const parts: string[] = [];
  if (ctx != null) parts.push(`ctx ${ctx.toFixed(0)}%`);
  if (snapshot.rateLimitsPresent && rl > 0) parts.push(`rl ${rl.toFixed(0)}%`);
  if (cost != null) parts.push(`$${cost.toFixed(2)}`);
  if (parts.length === 0) return null;
  return (
    <div className="px-2 pb-1 pl-4 text-[10px] text-neutral-600">
      {parts.join(" · ")}
    </div>
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
