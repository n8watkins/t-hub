// The sidebar CAPTAINS section body (docs/CAPTAIN-SIDEBAR-PRD.md, slice A):
// one row per PINNED captain in MRU order - the persistent supervision surface
// the anchor dropdown (a transient switcher) deliberately is not.
//
// Each row: the shared CaptainStatusDot, the shared display label, the
// workspace the captain's tile lives in (dimmed "tile not available" when the
// tab is popped out - same affordance as the dropdown rows), an honest
// SUBAGENT activity summary (Task-tool spawns from the supervision tree; real
// MCP-spawned crew are NOT linkable until the phase 2 server registry records
// spawnedBy - see the PRD), an amber outstanding-tasks badge, a thin context
// meter from the session's statusline snapshot (amber when rate-limited), and
// an attention roll-up that pulses the row amber while the captain session
// needs permission or a question. Clicking a row summons that captain (the
// same summonCaptain path as the dropdown); the chevron expands the existing
// SupervisionTreeView inline.
//
// Attention roll-up scope (slice A honesty note): SubagentNode carries only
// running/completed, so a CHILD cannot express needs-permission today - the
// roll-up covers the captain session's own status. Crew sessions join the
// roll-up in slice B when the registry links them.
import { useState } from "react";
import { useCaptain } from "../store/captain";
import {
  useSupervision,
  sessionStatusForTmux,
  isRateLimited,
} from "../store/supervision";
import { sessionNameForTerminal } from "../store/sessionContext";
import {
  CaptainStatusDot,
  useCaptainDisplayLabel,
  useWorkspaceNameForTerminal,
} from "./CaptainOverlay";
import { SupervisionTreeView } from "./SupervisionTree";
import { ContextMeter } from "./ContextMeter";
import { ChevronIcon } from "./SidebarChrome";

/** The list body: rows in the captain store's MRU order (index 0 = active). */
export function CaptainsList() {
  const captainIds = useCaptain((s) => s.captainIds);
  const activeCaptainId = useCaptain((s) => s.activeCaptainId);
  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {captainIds.map((id) => (
        <CaptainRow
          key={id}
          terminalId={id}
          active={id === activeCaptainId}
        />
      ))}
    </div>
  );
}

function CaptainRow({
  terminalId,
  active,
}: {
  terminalId: string;
  active: boolean;
}) {
  // Inline SupervisionTree expansion - per-row, transient (not persisted).
  const [expanded, setExpanded] = useState(false);

  const label = useCaptainDisplayLabel(terminalId);
  // The tab the captain's tile lives in; undefined = popped out / gone, the
  // same liveness lookup the dropdown rows use (shared hook, cannot drift).
  // Summon is then a store-level no-op, so the row must READ unavailable.
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  const hasTile = workspaceName != null;

  // Resolve the bound agent session via the statusline's tmux index, then the
  // supervision tree / status / snapshot for that session. All best-effort:
  // a captain with no session yet renders name + workspace only.
  const tmux = sessionNameForTerminal(terminalId);
  const sessionId = useSupervision((s) => s.sessionIdByTmux[tmux]);
  const tree = useSupervision((s) =>
    sessionId !== undefined ? s.trees[sessionId] : undefined,
  );
  const snap = useSupervision((s) =>
    sessionId !== undefined ? s.snapshots[sessionId] : undefined,
  );
  const status = useSupervision((s) => sessionStatusForTmux(s, tmux));

  // Attention roll-up: the captain needs the general. Scoped to the two
  // needs-input statuses per the PRD (rate-limit shows amber via the meter +
  // dot instead of a whole-row pulse).
  const attention = status === "needsQuestion" || status === "needsPermission";

  const running = tree?.children.filter((c) => c.state === "running").length ?? 0;
  const done = tree ? tree.children.length - running : 0;
  const tasks = tree?.outstandingTasks ?? 0;

  return (
    <div className="flex flex-col" data-captain-row={terminalId}>
      <div
        className="group relative flex items-center gap-1 rounded-lg transition-colors hover:bg-neutral-800/25"
        data-attention={attention || undefined}
      >
        {/* Amber attention pulse behind the row content (Tailwind animate-pulse
            on a tint layer - the dot's own halo pulse stays the fine signal).
            The visual tint is aria-hidden; the sr-only role="status" sibling
            carries the same signal to assistive tech (announced on change). */}
        {attention && (
          <>
            <span
              className="pointer-events-none absolute inset-0 animate-pulse rounded-lg"
              style={{
                backgroundColor: "color-mix(in srgb, #f59e0b 12%, transparent)",
              }}
              aria-hidden
            />
            <span role="status" className="sr-only">
              Captain {label} needs attention
            </span>
          </>
        )}

        {/* Expand: the existing supervision tree, inline under the row. */}
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          aria-expanded={expanded}
          aria-label={`${expanded ? "Collapse" : "Expand"} subagent activity - ${label}`}
          title={expanded ? "Collapse subagent activity" : "Expand subagent activity"}
          className="relative flex h-6 w-5 shrink-0 items-center justify-center rounded opacity-60 hover:opacity-100"
        >
          <ChevronIcon open={expanded} />
        </button>

        {/* Summon: the same path as the anchor dropdown rows. */}
        <button
          type="button"
          onClick={() => useCaptain.getState().summonCaptain(terminalId)}
          title={
            hasTile
              ? `Summon captain - ${label}`
              : `${label} - tile not available (tab popped out?)`
          }
          className="relative flex min-w-0 flex-1 items-center gap-2 py-1.5 pr-2 text-left"
          style={{ opacity: hasTile ? 1 : 0.5 }}
        >
          <CaptainStatusDot terminalId={terminalId} size={10} />
          <span className="flex min-w-0 flex-1 flex-col">
            <span
              className="min-w-0 truncate text-xs"
              style={{
                color: "var(--th-fg)",
                fontWeight: active ? 600 : 400,
              }}
            >
              {label}
              {active && (
                <span
                  className="ml-1.5 text-[9px] uppercase tracking-wide"
                  style={{ color: "var(--th-accent)" }}
                >
                  active
                </span>
              )}
            </span>
            {/* Second line: workspace + the honest subagent summary. */}
            <span
              className="min-w-0 truncate text-[10px]"
              style={{ color: "var(--th-fg-muted)" }}
              title={
                tree
                  ? "Subagent activity (Task-tool spawns). Separate crew sessions are not linked until the phase 2 captains registry."
                  : undefined
              }
            >
              {workspaceName ?? "tile not available"}
              {tree && (
                <> · subagents: {running} running · {done} done</>
              )}
            </span>
          </span>
          {tasks > 0 && (
            <span
              className="shrink-0 rounded px-1 text-[9px] font-semibold tabular-nums text-amber-400"
              style={{
                backgroundColor: "color-mix(in srgb, #f59e0b 15%, transparent)",
              }}
              title={`${tasks} outstanding background task${tasks === 1 ? "" : "s"}`}
            >
              {tasks} task{tasks === 1 ? "" : "s"}
            </span>
          )}
          {/* Accepted edge: a rate-limited snapshot WITHOUT contextUsedPct
              renders no meter at all (ContextMeter's null contract), so the
              amber meter cue is absent there - the status dot still carries
              the rateLimited overlay (attention amber), so the state is never
              silent. */}
          <ContextMeter
            usedPct={snap?.contextUsedPct ?? null}
            warn={isRateLimited(snap)}
          />
        </button>
      </div>

      {/* Inline supervision tree (the existing component; it renders its own
          muted hint when the captain has no session/tree yet). */}
      {expanded && (
        <div className="pl-4">
          <SupervisionTreeView sessionId={sessionId ?? ""} label={label} />
        </div>
      )}
    </div>
  );
}
