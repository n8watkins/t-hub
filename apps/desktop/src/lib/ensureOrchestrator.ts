// The default-orchestrator startup logic (ADOPT-ONLY, per the general's
// adversarial-audit hold on auto-spawn): on launch, ensure the deck's
// orchestrator points at a live session, WITHOUT ever spawning one.
//
// `~/.t-hub/orchestrator` is the canonical orchestrator home. We adopt by cwd
// suffix so we never need to resolve the (WSL-vs-Windows) absolute home path in
// the frontend - a live session whose cwd ends in `.t-hub/orchestrator` is THE
// orchestrator.

/** The canonical orchestrator home, as a path suffix. */
export const ORCHESTRATOR_CWD_SUFFIX = ".t-hub/orchestrator";

/** The user-facing NAME of the orchestrator agent. The orchestrator's cwd
 *  basename is "orchestrator", so the derived stable identity would read
 *  "orchestrator" - a bland, technical label. Wherever the DESIGNATED
 *  orchestrator is rendered to the user (the sidebar Agents row, the overlay
 *  switcher chip) we substitute this name instead. This is a display-only
 *  concern: the store's `orchestratorId` / `ensureOrchestrator` adopt logic is
 *  untouched, and a captain's own derived identity logic never sees it. */
export const ORCHESTRATOR_DISPLAY_NAME = "Cortana";

/** Normalize a cwd for suffix comparison: unify separators, drop trailing
 *  slashes, lowercase (Windows paths are case-insensitive; WSL paths here are
 *  lowercase for `.t-hub` anyway). */
function normalizeCwd(cwd: string): string {
  return cwd.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

/** True when `cwd` is the orchestrator home (`.../.t-hub/orchestrator`). */
export function isOrchestratorCwd(cwd: string | undefined | null): boolean {
  if (!cwd) return false;
  const n = normalizeCwd(cwd);
  return n === ORCHESTRATOR_CWD_SUFFIX || n.endsWith("/" + ORCHESTRATOR_CWD_SUFFIX);
}

/** A live terminal, as much as this logic needs. */
export interface OrchestratorTerminal {
  cwd?: string;
  state?: string;
}

/**
 * Resolve which terminal id (if any) should be the orchestrator - ADOPT ONLY,
 * never spawn. Precedence:
 *   1. the persisted orchestrator, if it is still a live terminal - keep it;
 *   2. else a live terminal whose cwd is the orchestrator home - adopt it;
 *   3. else null - leave it (no spawn).
 * Returns the id to DESIGNATE, or null to make no change. Pure + idempotent:
 * a second call with the same inputs designates the same id.
 */
export function resolveOrchestrator(
  orchestratorId: string | null,
  terminals: Record<string, OrchestratorTerminal>,
): string | null {
  if (orchestratorId && terminals[orchestratorId]) return orchestratorId;
  const match = Object.entries(terminals).find(([, t]) =>
    isOrchestratorCwd(t.cwd),
  );
  return match ? match[0] : null;
}

// --- Create Orchestrator (one-click commission) ------------------------------
// The USER-initiated create action (the sidebar affordance + the palette command).
// `ensureOrchestrator` above stays ADOPT-ONLY on startup (the auto-spawn hold); THIS
// is the explicit, deliberate create the adversarial audit asked for. The atomic
// 4-layer work (provision + control-tier spawn + resume/kickoff + cortana claim +
// retire-prior) is the BACKEND `commission_orchestrator` command's job; the frontend
// just fires it and focuses the resulting tile.

/** Guard against a double-click / two-in-flight commission (the singleton must not
 *  race two spawns). Mirrors the `recallInFlight` pattern in the workspace store. */
let commissionInFlight = false;

/** Bring the orchestrator tile forward: switch to the reserved Captains tab and
 *  focus it. The canonical "focus Cortana" primitive (see CaptainsList.revealAgent). */
async function focusTile(id: string): Promise<void> {
  const { useWorkspace } = await import("../store/workspace");
  const ws = useWorkspace.getState();
  ws.setActiveTab(ws.ensureCaptainsTab());
  ws.setFocus(id);
}

/**
 * Focus the currently-designated orchestrator, if any. The "Focus Cortana" half of
 * the sidebar affordance / palette command.
 */
export async function focusOrchestrator(): Promise<void> {
  const { useCaptain } = await import("../store/captain");
  const id = useCaptain.getState().orchestratorId;
  if (id) await focusTile(id);
}

/**
 * One-click Create Orchestrator: call the backend `commission_orchestrator` command
 * (which adopts a live one / re-spawn-resumes a dead one / cold-starts a fresh one,
 * never duplicating), then designate + focus the resulting tile. `forceRespawn`
 * drives the restart stale-token repair / power-user re-key (Case B/C even over a
 * live tile). Idempotent under a double-click via `commissionInFlight`.
 */
export async function commissionOrchestrator(opts?: {
  forceRespawn?: boolean;
}): Promise<string | null> {
  if (commissionInFlight) return null;
  commissionInFlight = true;
  try {
    const { controlRequest } = await import("../ipc/controlClient");
    const res = (await controlRequest(
      "commission_orchestrator",
      opts?.forceRespawn ? { forceRespawn: true } : {},
    )) as { terminalId?: unknown } | null;
    const id =
      res && typeof res.terminalId === "string" ? res.terminalId : null;
    if (!id) return null;
    // Designate LOCALLY (M1): the backend `commission_orchestrator` already claimed
    // the cortana role for this tile, carefully preserving the resume anchor. Driving
    // the PUBLIC `setOrchestratorId` here would re-release + re-claim server-side and
    // WIPE that anchor (a childless release drops `claude_uuid`; the fresh re-claim has
    // no UUID yet), so an app restart in the heal window would lose transcript
    // continuity. `designateOrchestratorLocal` just mirrors the state + places the tile.
    const { useCaptain } = await import("../store/captain");
    useCaptain.getState().designateOrchestratorLocal(id);
    await focusTile(id);
    return id;
  } catch (err) {
    console.error("commissionOrchestrator failed", err);
    return null;
  } finally {
    commissionInFlight = false;
  }
}
