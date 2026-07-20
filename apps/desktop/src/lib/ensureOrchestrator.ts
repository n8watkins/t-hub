// Pure frontend designation helpers for the default orchestrator.
// Process creation and recovery are owned by the trusted backend
// `reconcile_cortana` command; this module only resolves the live tile to display.
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
 * Resolve which terminal id (if any) should be the orchestrator after the
 * backend recovery transaction has completed. Precedence:
 *   1. the persisted orchestrator, if it is still a live terminal - keep it;
 *   2. else a live terminal whose cwd is the orchestrator home - adopt it;
 *   3. else null - leave it for the backend result to populate.
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
