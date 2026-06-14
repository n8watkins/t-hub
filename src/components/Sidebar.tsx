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
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { SupervisionTreeBody } from "./SupervisionTree";
import { StatusBadge, statusLabel } from "./StatusBadge";
import { WslHealth } from "./WslHealth";
import { HookInstallPanel } from "./HookInstallPanel";
import type { StatusSnapshot, SupervisionTree } from "../ipc/model";

/**
 * The sidebar's 3-state collapse mode (App owns + persists it; #1):
 *  - "full": the resizable full-width supervision surface (the original view).
 *  - "rail": a thin ~48px strip showing just iconic section markers + a compact
 *    workspace list, "barely showing" but still useful for switching tabs.
 *  - "hidden": not rendered at all (App skips <Sidebar> entirely).
 * The cycle full -> rail -> hidden -> full is driven by App's onToggleSidebar.
 */
export type SidebarMode = "full" | "rail" | "hidden";

/** Pixel width of the rail strip (kept in sync with App's RAIL width). */
export const SIDEBAR_RAIL_WIDTH = 48;

export interface SidebarProps {
  /** Called when the user clicks an attention-queue row or a tree header. */
  onSelectSession?: (sessionId: string) => void;
  /** Collapse mode (#1). "hidden" is handled by App (it skips render), so the
   *  component itself only ever sees "full" or "rail"; defaults to "full". */
  mode?: SidebarMode;
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
  mode = "full",
  width = 256,
  agentBin = "termhub-agent",
}: SidebarProps) {
  // Workspace tabs (read-only, #2): list every tab with its tile count and let
  // a click activate it. We never mutate the store beyond setActiveTab.
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);

  // Rail mode: a thin, iconic strip. Render before pulling the heavier
  // supervision selectors below stays cheap, but hooks must run unconditionally,
  // so derive everything and branch on render.
  if (mode === "rail") {
    return (
      <SidebarRail
        width={width}
        tabs={tabs}
        activeTabId={activeTabId}
        setActiveTab={setActiveTab}
      />
    );
  }
  return (
    <SidebarFull
      onSelectSession={onSelectSession}
      width={width}
      agentBin={agentBin}
      tabs={tabs}
      activeTabId={activeTabId}
      setActiveTab={setActiveTab}
    />
  );
}

interface FullProps {
  onSelectSession?: (sessionId: string) => void;
  width: number;
  agentBin: string;
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
}

function SidebarFull({
  onSelectSession,
  width,
  agentBin,
  tabs,
  activeTabId,
  setActiveTab,
}: FullProps) {
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
      {/* 0. Workspaces (#2) — the user's tabs with tile counts, active one
          highlighted; a click activates the tab. Gives the otherwise-empty
          sidebar immediate utility. */}
      <section className="border-b border-neutral-800">
        <Header>Workspaces</Header>
        <WorkspaceList
          tabs={tabs}
          activeTabId={activeTabId}
          setActiveTab={setActiveTab}
        />
      </section>

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

/** The Workspaces list (full mode): one row per tab = name + tile count, the
 *  active tab accent-highlighted; clicking activates it via setActiveTab (#2). */
function WorkspaceList({
  tabs,
  activeTabId,
  setActiveTab,
}: {
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
}) {
  if (tabs.length === 0) return <Muted>No workspaces.</Muted>;
  return (
    <ul>
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        const count = tab.order.length;
        return (
          <li key={tab.id}>
            <button
              type="button"
              onClick={() => setActiveTab(tab.id)}
              className="flex w-full items-center gap-2 px-2 py-1 text-left text-xs hover:bg-neutral-900"
              style={
                active
                  ? { backgroundColor: "var(--th-accent)", color: "var(--th-fg)" }
                  : { color: "var(--th-fg-muted)" }
              }
              title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"}`}
              aria-current={active ? "true" : undefined}
            >
              <span className="min-w-0 flex-1 truncate">{tab.name}</span>
              <span className="shrink-0 tabular-nums opacity-70">{count}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * Rail mode (#1): a thin ~48px iconic strip — "barely showing" but still useful.
 * It stacks one square per workspace tab (its initial + a tiny tile count) so the
 * user can still switch tabs, then a small column of section glyphs (Attention /
 * Sessions / Hooks / WSL) as a hint of what the full sidebar holds.
 */
function SidebarRail({
  width,
  tabs,
  activeTabId,
  setActiveTab,
}: {
  width: number;
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
}) {
  return (
    <aside
      className="flex h-full shrink-0 flex-col items-center gap-1 border-r py-2"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {tabs.map((tab) => {
        const active = tab.id === activeTabId;
        const count = tab.order.length;
        const initial = (tab.name.trim()[0] ?? "?").toUpperCase();
        return (
          <button
            key={tab.id}
            type="button"
            onClick={() => setActiveTab(tab.id)}
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"}`}
            aria-current={active ? "true" : undefined}
            className="relative flex h-8 w-8 items-center justify-center rounded text-xs font-semibold hover:opacity-90"
            style={{
              backgroundColor: active ? "var(--th-accent)" : "transparent",
              color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
              border: active ? undefined : "1px solid var(--th-border)",
            }}
          >
            {initial}
            {count > 0 && (
              <span
                className="absolute -right-0.5 -top-0.5 min-w-[12px] rounded-full px-0.5 text-center text-[8px] leading-[12px]"
                style={{
                  backgroundColor: "var(--th-border)",
                  color: "var(--th-fg)",
                }}
              >
                {count}
              </span>
            )}
          </button>
        );
      })}
      {tabs.length === 0 && (
        <div
          className="text-[9px]"
          style={{ color: "var(--th-fg-muted)" }}
          title="No workspaces"
        >
          —
        </div>
      )}
      {/* Section hints: glyphs standing in for the full sidebar's sections. */}
      <div
        className="mt-auto flex flex-col items-center gap-1 pt-2 text-sm"
        style={{ color: "var(--th-fg-muted)" }}
        aria-hidden
      >
        <span title="Attention">!</span>
        <span title="Sessions">≡</span>
        <span title="Hooks">⚓</span>
        <span title="WSL">◷</span>
      </div>
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
