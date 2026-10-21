import {
  useCaptain
} from "../store/captain";
import { tabIdForTerminal, useWorkspace } from "../store/workspace";

/** Resolve one user-facing Captain name from durable registry identity only. */
export function stableCaptainIdentity(
  displayName: string | undefined,
  shipSlug: string | undefined,
  terminalId: string,
): string {
  const named = (displayName ?? "").trim();
  if (named) return named;
  const slug = (shipSlug ?? "").trim();
  if (slug) return slug;
  return terminalId.slice(0, 8);
}

/** The user-facing display label for a captain terminal, resolved to the STABLE
 *  identity (rename -> workspace tab name -> cwd basename -> short id; NEVER the
 *  Claude title). Reads the raw user rename from `userLabels` (not the merged
 *  `labels`, which folds in Claude titles). Shared by the overlay switcher, the
 *  titlebar dropdown, the deck tiles, and the palette so a captain reads
 *  identically everywhere. */
export function useCaptainDisplayLabel(terminalId: string): string {
  const claim = useCaptain((s) => s.claims[terminalId]);
  return stableCaptainIdentity(claim?.displayName, claim?.shipSlug, terminalId);
}

/** The NAME of the workspace tab holding `terminalId`'s tile, or undefined
 *  when the tile is gone (tab popped out to a satellite / terminal killed) -
 *  undefined doubles as the liveness affordance (dim + "tile not available").
 *  Shared by the titlebar dropdown rows and the sidebar captain rows so the
 *  workspace-context lookup cannot drift between the two surfaces. Built on
 *  tabIdForTerminal (memoized O(1) per tile) rather than a per-row scan. */
export function useWorkspaceNameForTerminal(
  terminalId: string,
): string | undefined {
  return useWorkspace((s) => {
    const tabId = tabIdForTerminal(s, terminalId);
    if (tabId === undefined) return undefined;
    return s.tabs.find((t) => t.id === tabId)?.name;
  });
}
