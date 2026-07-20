// Captain notification attribution: NAME the captain/ship a notification came
// from, so the general can tell WHICH captain wants attention instead of just
// "a session needs your input". Resolves a Claude session id -> its tile
// (`th_<id>`) -> the captains registry (the designated orchestrator or a pinned
// captain), then produces the human name the general already sees for it.
//
// CONTENT/NAMING ONLY. This never decides WHETHER to speak or notify - that
// gating lives in lib/voiceAnnounce.ts and lib/notify.ts. Callers that have
// ALREADY decided to fire a notification ask this for the subject to put in it,
// so this stays clear of the voice-gate (Scribe) semantics entirely.
import { useSupervision } from "../store/supervision";
import { useCaptain } from "../store/captain";
import { ORCHESTRATOR_DISPLAY_NAME } from "./ensureOrchestrator";

/** The tile (terminal) id backing a Claude session id, via the supervision
 *  reverse index (`sessionIdByTmux` maps `th_<id>` -> sessionId). Null when the
 *  session has no resolvable tmux tile. */
export function terminalIdForSession(sessionId: string): string | null {
  const { sessionIdByTmux } = useSupervision.getState();
  const tmux = Object.entries(sessionIdByTmux).find(
    ([, sid]) => sid === sessionId,
  )?.[0];
  if (!tmux || !tmux.startsWith("th_")) return null;
  return tmux.slice("th_".length);
}

export interface CaptainAttribution {
  /** The designated orchestrator reads by its brand name (no "Captain" prefix);
   *  a regular captain reads as `Captain <name>`. */
  isOrchestrator: boolean;
  /** The human name to announce: the orchestrator brand, else the captain's
   *  stable identity (user rename > cwd folder > workspace tab > ship slug > id).
   *  Deliberately NOT the volatile Claude title (which reflects typed input). */
  name: string;
}

/** Attribution for the captain backing `sessionId`, or null when that session is
 *  NOT a captain (a regular crew/work session keeps the un-attributed line). */
export function captainAttributionForSession(
  sessionId: string,
): CaptainAttribution | null {
  const terminalId = terminalIdForSession(sessionId);
  if (!terminalId) return null;
  const cap = useCaptain.getState();
  if (cap.orchestratorId === terminalId) {
    return { isOrchestrator: true, name: ORCHESTRATOR_DISPLAY_NAME };
  }
  if (!cap.captainIds.includes(terminalId)) return null;
  const claim = cap.claims[terminalId];
  const name =
    claim?.displayName?.trim() ||
    claim?.shipSlug?.trim() ||
    terminalId.slice(0, 8);
  return { isOrchestrator: false, name };
}

/** The spoken/visual SUBJECT for a captain session ("Captain alpha", or the
 *  orchestrator's brand name), or null when the session is not a captain. */
export function captainSubjectForSession(sessionId: string): string | null {
  const a = captainAttributionForSession(sessionId);
  if (!a) return null;
  return a.isOrchestrator ? a.name : `Captain ${a.name}`;
}
