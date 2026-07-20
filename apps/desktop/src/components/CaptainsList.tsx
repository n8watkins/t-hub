// The sidebar CAPTAINS section body (docs/CAPTAIN-SIDEBAR-PRD.md, slice B on
// top of the captain-rows round): one row per PINNED captain - the persistent
// supervision surface the anchor dropdown (a transient switcher) deliberately
// is not.
//
// Rows are IDENTITY-FIRST per the general's feedback: the prominent line is
// the user's rename (or, unnamed, the repo/worktree the captain lives in) -
// never the derived last-command text. The second line is location context
// (the workspaces the captain CONTROLS, from the phase 2 registry's
// workspaceTabIds, falling back to the tile's own tab; + worktree branch). The
// activity line below shows REAL crew (the sessions the captain spawned, from
// the registry's spawnedBy links - NOT a count of Task-tool subagents),
// de-emphasized. Rows are ordered WORKSPACE-FIRST: captains whose tile lives in
// the ACTIVE workspace tab float to the top (accent-marked), so switching tabs
// surfaces the relevant captain; MRU order holds within each group.
//
// Rename: the hover pencil swaps the identity block for an input (Enter
// commits, Esc cancels, blur commits - plain input handling, mirroring
// WorkspacesList's double-click rename; lib/escOverlays is untouched). Known
// shared edge (same as the WorkspacesList rename): while an esc surface is
// armed (e.g. a tile is fullscreen), Canvas's window-capture listener consumes
// Esc before this input sees it - blur-commit still closes the edit sanely.
// Committing goes through the workspace store's persisted setTerminalLabel,
// which every label surface (tiles, dropdown, overlay chips, palette) already
// reads first.
//
// Attention roll-up (slice B): the row pulses amber while the captain's OWN
// session OR any of its crew needs permission or a question - a crewmate stuck
// on a prompt bubbles amber onto its captain. Expanding a captain lists its
// crewmates (each with its own status dot) above the captain's own subagent
// tree.
import { useState } from "react";
import { Pencil } from "lucide-react";
import { useCaptain } from "../store/captain";
import { useWorkspace } from "../store/workspace";
import {
  useSupervision,
  sessionStatusForTmux,
  isRateLimited,
} from "../store/supervision";
import { sessionNameForTerminal } from "../store/sessionContext";
import {
  CaptainStatusDot,
  useWorkspaceNameForTerminal,
  stableCaptainIdentity,
} from "./CaptainOverlay";
import { useCrewSummary } from "../hooks/useCrewSummary";
import { SupervisionTreeView } from "./SupervisionTree";
import { ContextMeter } from "./ContextMeter";
import { ChevronIcon } from "./SidebarChrome";
import { OrchestratorCrownIcon } from "./OrchestratorCrownIcon";
import { ORCHESTRATOR_DISPLAY_NAME } from "../lib/ensureOrchestrator";
import { StartAgentDialog } from "./StartAgentDialog";

/** Navigate to the reserved Captains workspace tab and focus an agent's live
 *  terminal tile - the sidebar-row click behavior now that agents live as
 *  ordinary tiles in the Captains tab (no custom deck). */
function revealAgent(terminalId: string): void {
  const ws = useWorkspace.getState();
  ws.setActiveTab(ws.ensureCaptainsTab());
  ws.setFocus(terminalId);
}

/** Path segments of a cwd, tolerant of either separator and trailing slashes. */
function cwdParts(cwd: string): string[] {
  return cwd
    .replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean);
}

/** The worktree/branch context for a cwd - the `wt-<branch>` segment when the
 *  repo convention is in play, else "" (mirrors RecentList's cwdWorktree; the
 *  parent-folder fallback is skipped here because the row already leads with
 *  the basename). */
function cwdBranch(cwd: string): string {
  const parts = cwdParts(cwd);
  for (let i = parts.length - 1; i >= 0; i -= 1) {
    if (/^wt-/.test(parts[i])) return parts[i].replace(/^wt-/, "");
  }
  return "";
}

/** The sidebar's top-of-hierarchy ORCHESTRATOR row (Cortana): the SAME rich
 *  agent row the captains get - live status dot, supervision status, workspace
 *  context chips, claim / needs-input treatment - differing only by (a) the
 *  fixed "Cortana" name and (b) the crown badge marking it as the orchestrator.
 *  It renders the shared `AgentRow` body so parity can never drift. Clicking
 *  navigates to the Captains tab and focuses its tile. Renders only when an
 *  orchestrator is designated AND it is not already shown as a pinned captain in
 *  the list below (dedupe the hierarchy). */
export function OrchestratorRow() {
  const orchestratorId = useCaptain((s) => s.orchestratorId);
  const isAlsoCaptain = useCaptain(
    (s) => orchestratorId != null && s.captainIds.includes(orchestratorId),
  );
  const activeCaptainId = useCaptain((s) => s.activeCaptainId);
  // The active tab's tile order: the orchestrator row floats + flags the accent
  // marker when its tile lives in the active workspace, exactly like captains.
  const inActiveWorkspace = useWorkspace((s) => {
    if (orchestratorId == null) return false;
    return (
      s.tabs
        .find((t) => t.id === s.activeTabId)
        ?.order.includes(orchestratorId) ?? false
    );
  });
  if (!orchestratorId || isAlsoCaptain) return null;
  return (
    <div className="px-2 pt-1" data-orchestrator-row>
      <AgentRow
        terminalId={orchestratorId}
        active={orchestratorId === activeCaptainId}
        inActiveWorkspace={inActiveWorkspace}
        orchestrator
      />
    </div>
  );
}

export function CaptainsList() {
  const captainIds = useCaptain((s) => s.captainIds);
  const activeCaptainId = useCaptain((s) => s.activeCaptainId);
  // The active tab's tile order: a stable array reference until THAT tab's
  // contents change, so this selector doesn't churn re-renders on unrelated
  // store writes.
  const activeTabOrder = useWorkspace(
    (s) => s.tabs.find((t) => t.id === s.activeTabId)?.order,
  );
  const inActiveTab = (id: string) => activeTabOrder?.includes(id) ?? false;
  const ordered = [
    ...captainIds.filter(inActiveTab),
    ...captainIds.filter((id) => !inActiveTab(id)),
  ];
  return (
    <div className="flex flex-col gap-0.5 px-2 py-1">
      {ordered.map((id) => (
        <AgentRow
          key={id}
          terminalId={id}
          active={id === activeCaptainId}
          inActiveWorkspace={inActiveTab(id)}
        />
      ))}
    </div>
  );
}

/** ONE agent row - shared by the pinned captains AND the top-of-hierarchy
 *  orchestrator (Cortana) so their live status / context render IDENTICALLY.
 *  The only differences the `orchestrator` flag introduces are cosmetic: the
 *  fixed "Cortana" display name, the crown badge (in place of the pencil
 *  rename), and a slightly larger status dot. Everything else - the supervision
 *  status dot color, workspace-tab context, crew summary, needs-input amber
 *  pulse, context meter, workspace-relevant accent marker - is one code path. */
function AgentRow({
  terminalId,
  active,
  inActiveWorkspace,
  orchestrator = false,
}: {
  terminalId: string;
  active: boolean;
  /** True when the agent's tile lives in the ACTIVE workspace tab - the row
   *  floats to the top and carries the accent marker. */
  inActiveWorkspace: boolean;
  /** True for the designated orchestrator (Cortana): fixed name + crown badge,
   *  rename suppressed. Defaults false (a plain pinned captain). */
  orchestrator?: boolean;
}) {
  // Inline expansion (crew sub-rows + supervision tree) - per-row, transient.
  const [expanded, setExpanded] = useState(false);
  // Inline rename (WorkspacesList's editing/draft/commit shape). Never armed
  // for the orchestrator - its name is the fixed brand label.
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [startAgentOpen, setStartAgentOpen] = useState(false);

  const cwd = useWorkspace((s) => s.terminals[terminalId]?.cwd);
  // The tab the captain's tile lives in; undefined = popped out / gone, the
  // same liveness lookup the dropdown rows use (shared hook, cannot drift).
  // Summon is then a store-level no-op, so the row must READ unavailable.
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  const hasTile = workspaceName != null;
  // STABLE identity: rename -> cwd basename -> workspace tab name (NEVER the
  // volatile Claude title). cwd beats the tab name so unrelated captains
  // sharing one tab stay distinct. Shared with the overlay. The orchestrator
  // shows its fixed brand name ("Cortana") instead of the derived basename
  // (which would read the bland "orchestrator").
  const claim = useCaptain((s) => s.claims[terminalId]);
  const identity = orchestrator
    ? ORCHESTRATOR_DISPLAY_NAME
    : stableCaptainIdentity(claim?.displayName, claim?.shipSlug, terminalId);
  const branch = cwd ? cwdBranch(cwd) : "";
  const roleLabel = orchestrator ? "orchestrator" : "captain";

  // Phase 2: the captain's server-registry claim - the workspaces it controls.
  // Resolve the captain's bound agent session via the statusline's tmux index,
  // then the supervision tree / status / snapshot for that session. All
  // best-effort: a captain with no session yet renders identity + location only.
  const tmux = sessionNameForTerminal(terminalId);
  const sessionId = useSupervision((s) => s.sessionIdByTmux[tmux]);
  const tree = useSupervision((s) =>
    sessionId !== undefined ? s.trees[sessionId] : undefined,
  );
  const snap = useSupervision((s) =>
    sessionId !== undefined ? s.snapshots[sessionId] : undefined,
  );
  const status = useSupervision((s) => sessionStatusForTmux(s, tmux));

  // Crew activity from the registry (slice B: real crew, not subagents).
  const crew = useCrewSummary(terminalId);

  // The workspaces the captain CONTROLS (registry workspaceTabIds -> names),
  // falling back to the tile's own tab when no claim has synced yet.
  const tabs = useWorkspace((s) => s.tabs);
  const controlling = (claim?.workspaceTabIds ?? [])
    .map((id) => tabs.find((t) => t.id === id)?.name)
    .filter((n): n is string => n != null && n !== "");
  const workspaceText =
    controlling.length > 0
      ? controlling.join(", ")
      : (workspaceName ?? "tile not available");

  // Attention roll-up: the captain OR any crewmate needs the general. Scoped to
  // the two needs-input statuses per the PRD (rate-limit shows amber via the
  // meter + dot instead of a whole-row pulse).
  const ownAttention =
    status === "needsQuestion" || status === "needsPermission";
  const attention = ownAttention || crew.needsInput;

  const tasks = tree?.outstandingTasks ?? 0;

  const commitRename = () => {
    setEditing(false);
    const displayName = draft.trim();
    if (!displayName || displayName === identity || !claim) return;
    void import("../ipc/controlClient")
      .then((client) =>
        client.controlRequest("rename_captain", {
          captainSessionId: terminalId,
          displayName,
        }),
      )
      .catch(() => {});
  };

  return (
    <div className="flex flex-col" data-captain-row={terminalId}>
      <div
        className="group relative flex items-center gap-1 rounded-lg transition-colors hover:bg-neutral-800/25"
        data-attention={attention || undefined}
        data-in-active-workspace={inActiveWorkspace || undefined}
        style={
          inActiveWorkspace
            ? {
                // The workspace-relevant marker: an accent inset bar + faint
                // tint (the RecentList color-bar idiom), so switching tabs
                // visibly floats + flags this captain.
                boxShadow: "inset 2px 0 0 0 var(--th-accent)",
                backgroundColor:
                  "color-mix(in srgb, var(--th-accent) 6%, transparent)",
              }
            : undefined
        }
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
              {identity} needs attention
            </span>
          </>
        )}

        {/* Expand: crew sub-rows + the captain's own supervision tree, inline. */}
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          aria-expanded={expanded}
          aria-label={`${expanded ? "Collapse" : "Expand"} crew and subagents - ${identity}`}
          title={expanded ? "Collapse crew and subagents" : "Expand crew and subagents"}
          className="relative flex h-6 w-5 shrink-0 items-center justify-center rounded opacity-60 hover:opacity-100"
        >
          <ChevronIcon open={expanded} />
        </button>

        {editing ? (
          // Rename in place of the summon block: Enter commits, Esc cancels,
          // blur commits (plain input handling; the escOverlays window
          // listener is not armed unless an overlay is up).
          <div className="relative flex min-w-0 flex-1 items-center gap-2 py-2 pr-2">
            <CaptainStatusDot terminalId={terminalId} size={11} />
            <input
              autoFocus
              value={draft}
              placeholder={identity}
              aria-label={`Rename ${roleLabel} - ${identity}`}
              onChange={(e) => setDraft(e.target.value)}
              onFocus={(e) => e.target.select()}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename();
                else if (e.key === "Escape") setEditing(false);
              }}
              spellCheck={false}
              className="min-w-0 flex-1 rounded bg-transparent px-1 py-0.5 text-xs outline-none"
              style={{
                color: "var(--th-fg)",
                border: "1px solid var(--th-accent)",
              }}
            />
          </div>
        ) : (
          /* Click navigates to the Captains tab and focuses this agent's tile. */
          <button
            type="button"
            onClick={() => revealAgent(terminalId)}
            title={
              hasTile
                ? `Open in Captain Workspace - ${identity}${orchestrator ? " (orchestrator)" : ""}`
                : `${identity} - terminal not available (tab popped out?)`
            }
            className="relative flex min-w-0 flex-1 items-center gap-2 py-2 pr-1 text-left"
            style={{ opacity: hasTile ? 1 : 0.5 }}
          >
            <CaptainStatusDot terminalId={terminalId} size={11} />
            {orchestrator && (
              // The special orchestrator marker: the crown sits between the live
              // status dot (parity signal) and the name, accent-colored so it
              // reads in both themes. title/aria carry the role for a11y.
              <span
                className="shrink-0"
                style={{ color: "var(--th-accent)" }}
                title="Orchestrator - commands the fleet"
                aria-label="Orchestrator"
              >
                <OrchestratorCrownIcon size={13} />
              </span>
            )}
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              {/* IDENTITY line: the rename, else the repo folder - prominent. */}
              <span
                className="min-w-0 truncate text-[13px]"
                style={{
                  color: "var(--th-fg)",
                  fontWeight: active ? 600 : 500,
                }}
              >
                {identity}
                {active && (
                  <span
                    className="ml-1.5 text-[9px] font-semibold uppercase tracking-wide"
                    style={{ color: "var(--th-accent)" }}
                  >
                    active
                  </span>
                )}
              </span>
              {/* LOCATION line: controlling workspaces + worktree branch. */}
              <span
                className="min-w-0 truncate text-[10px]"
                style={{ color: "var(--th-fg-muted)" }}
              >
                {workspaceText}
                {branch && <> · ⎇ {branch}</>}
              </span>
              {/* ACTIVITY line (de-emphasized): the REAL crew summary (registry
                  spawnedBy links) - supersedes the turn-scoped subagent summary
                  on the row; the subagent tree still shows in the expansion. */}
              {crew.members.length > 0 && (
                <span
                  className="min-w-0 truncate text-[10px] opacity-75"
                  style={{ color: "var(--th-fg-muted)" }}
                  title="Crew: the sessions this captain spawned (registry spawnedBy links), by running/done."
                >
                  crew: {crew.running} running · {crew.done} done
                </span>
              )}
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
        )}

        {/* Rename affordance: hover-revealed pencil (a separate control, not
            double-click, so a rename can never accidentally summon). Suppressed
            for the orchestrator - "Cortana" is a fixed brand label, not a
            per-session rename. */}
        {!editing && !orchestrator && (
          <button
            type="button"
            aria-label={`Rename captain - ${identity}`}
            title="Rename captain"
            onClick={() => {
              // Seed with the CURRENT override (not the derived identity) so
              // committing an untouched empty draft clears back to derived.
              setDraft(identity);
              setEditing(true);
            }}
            className="relative flex h-6 w-5 shrink-0 items-center justify-center rounded opacity-0 transition-opacity hover:bg-neutral-700/40 focus:opacity-100 group-hover:opacity-100"
            style={{ color: "var(--th-fg-muted)" }}
          >
            <Pencil size={11} className="pointer-events-none" aria-hidden />
          </button>
        )}
        {!editing && !orchestrator && claim?.projectId && cwd && (
          <button
            type="button"
            aria-label={`Start agent for ${identity}`}
            title="Start agent"
            onClick={(event) => {
              event.stopPropagation();
              setStartAgentOpen(true);
            }}
            className="relative flex h-6 shrink-0 items-center rounded px-1 text-[10px] opacity-0 transition-opacity hover:bg-neutral-700/40 focus:opacity-100 group-hover:opacity-100"
            style={{ color: "var(--th-fg-muted)" }}
          >
            + agent
          </button>
        )}
      </div>

      {/* Expanded: the real crew (registry spawnedBy links), each with its own
          status dot, then the captain's own subagent tree (Task-tool spawns -
          a distinct thing from crew sessions). */}
      {expanded && (
        <div className="flex flex-col gap-0.5 pl-4">
          {crew.members.length > 0 && (
            <div className="flex flex-col">
              {crew.members.map((m) => (
                <CrewRow key={m.id} terminalId={m.id} />
              ))}
            </div>
          )}
          <SupervisionTreeView sessionId={sessionId ?? ""} label={identity} />
        </div>
      )}
      <StartAgentDialog
        open={startAgentOpen}
        captainSessionId={terminalId}
        directory={cwd ?? ""}
        onClose={() => setStartAgentOpen(false)}
        onStarted={() => setExpanded(true)}
      />
    </div>
  );
}

/** One crewmate sub-row under an expanded captain (slice B): the shared status
 *  dot + identity + the workspace tab it lives in. Its own component so it can
 *  use the shared per-terminal hooks without a hook loop; identity-first like
 *  the captain rows (rename, else cwd basename). */
function CrewRow({ terminalId }: { terminalId: string }) {
  const userLabel = useWorkspace((s) => s.userLabels[terminalId]);
  const cwd = useWorkspace((s) => s.terminals[terminalId]?.cwd);
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  const folder = cwd
    ?.replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean)
    .at(-1);
  const identity =
    userLabel?.trim() || folder || workspaceName?.trim() || terminalId.slice(0, 8);
  return (
    <div
      className="flex items-center gap-2 py-0.5"
      data-crew-row={terminalId}
    >
      <CaptainStatusDot terminalId={terminalId} size={8} />
      <span
        className="min-w-0 flex-1 truncate text-[10px]"
        style={{ color: "var(--th-fg)" }}
      >
        {identity}
      </span>
      {workspaceName != null && (
        <span
          className="shrink-0 truncate text-[9px]"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {workspaceName}
        </span>
      )}
    </div>
  );
}
