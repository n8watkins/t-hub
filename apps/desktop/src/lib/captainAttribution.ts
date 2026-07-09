// Attribute a terminal/session to the captain (ship) it belongs to, as a human
// label for notifications — e.g. "Captain scribe", "Cortana".
//
// RECONCILE-AT-MERGE (PR #44, branch ui-batch): PR #44 lands the canonical
// apps/desktop/src/lib/captainAttribution.ts (the "Cortana crown" work) with the
// same purpose — resolve a session to "Captain <ship>" / "Cortana". This branch
// was cut from main BEFORE #44 merged, so this is a minimal LOCAL resolver with
// the same signature/behavior to unblock the auto-continue resume notification.
// At merge, prefer #44's version and keep the single call site in
// lib/autoContinueMount.ts (captainAttribution(id) → string | null). See the PR
// body for the coupling note.
import type { TerminalId } from "../ipc/types";
import { useCaptain } from "../store/captain";
import { useWorkspace, tabIdForTerminal } from "../store/workspace";

/**
 * The captain/ship a terminal belongs to, as a display label, or null when the
 * tile has no captain attribution (the caller falls back to the tile's own
 * label). Resolution order, most specific first:
 *   1. the designated ORCHESTRATOR — the general's own top agent — speaks as
 *      "Cortana";
 *   2. the tile IS a pinned captain → "Captain <ship>";
 *   3. the tile is a captain's CREW (spawnedBy) → "Captain <ship>";
 *   4. the tile lives in a captain's owned WORKSPACE TAB → "Captain <ship>".
 * Reads the stores via getState() (a plain function, not a hook), so imperative
 * callers like the auto-continue mount can use it off the hot path.
 */
export function captainAttribution(id: TerminalId): string | null {
  const cap = useCaptain.getState();
  if (cap.orchestratorId === id) return "Cortana";

  const claims = cap.claims;
  const own = claims[id];
  if (own) return `Captain ${own.shipSlug}`;

  const tabId = tabIdForTerminal(useWorkspace.getState(), id);
  for (const c of Object.values(claims)) {
    if (c.crew.includes(id)) return `Captain ${c.shipSlug}`;
    if (tabId && c.workspaceTabIds.includes(tabId)) return `Captain ${c.shipSlug}`;
  }
  return null;
}
