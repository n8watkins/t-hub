// Shared crew-summary hook (captain-chat phase 2, slice B data): resolves the
// real crew a captain spawned (from the server captains registry's `crew` list)
// to per-crewmate live status, and rolls it up to running/done counts + a
// needs-input flag. Used by BOTH the sidebar captain rows (CaptainsList) and the
// captains deck tiles so the two surfaces read one source of truth.
import { useMemo } from "react";
import { useCaptain, type CrewRef } from "../store/captain";
import { useSupervision } from "../store/supervision";
import type { SessionStatus } from "../ipc/model";
import { sessionNameForTerminal } from "../store/sessionContext";

/** Raw reducer statuses that mean a crew session is MID-TURN (mirrors the
 *  supervision store's private ACTIVE_TURN set - kept local so this reads
 *  without reaching into it). */
const CREW_RUNNING: ReadonlySet<SessionStatus> = new Set<SessionStatus>([
  "working",
  "needsQuestion",
  "needsPermission",
  "waitingOnSubagents",
]);

/** Stable empty crew list so the memo's dep identity does not churn for a
 *  captain with no claim/crew yet. */
const NO_CREW: readonly CrewRef[] = [];

/** One crewmate's rolled-up state. */
export interface CrewMember {
  id: string;
  /** The crew session has reported a status (vs. just-spawned, not yet known). */
  known: boolean;
  /** Mid-turn (working / waiting / needs-input). */
  running: boolean;
  /** Blocked on the general (needs-permission / needs-question). */
  needsInput: boolean;
}

export interface CrewSummary {
  /** Per-crewmate rolled-up state, in registry order. */
  members: CrewMember[];
  /** How many crew are mid-turn. */
  running: number;
  /** How many crew have reported a status and are no longer mid-turn (a
   *  just-spawned crewmate with no status yet counts as neither). */
  done: number;
  /** Any crewmate blocked on the general (drives the attention roll-up). */
  needsInput: boolean;
}

/**
 * Resolve the crew summary for a captain terminal id. Subscribes only to the two
 * supervision maps it needs (statuses + tmux index), so a statusline snapshot
 * storm does not re-render every consumer.
 */
export function useCrewSummary(terminalId: string): CrewSummary {
  const claim = useCaptain((s) => s.claims[terminalId]);
  const crew = claim?.crew ?? NO_CREW;
  const statuses = useSupervision((s) => s.statuses);
  const sessionIdByTmux = useSupervision((s) => s.sessionIdByTmux);

  return useMemo(() => {
    // A crew whose OWN tile died is marked `removed` (item-2 §2.4); it is gone, so
    // it drops out of the live crew summary rather than lingering as a dead row.
    const members: CrewMember[] = crew
      .filter((c) => c.state?.kind !== "removed")
      .map((c) => {
        const id = c.terminalId;
        const sid = sessionIdByTmux[sessionNameForTerminal(id)];
        const st = sid !== undefined ? statuses[sid] : undefined;
        return {
          id,
          known: st !== undefined,
          running: st !== undefined && CREW_RUNNING.has(st),
          needsInput: st === "needsQuestion" || st === "needsPermission",
        };
      });
    const running = members.filter((c) => c.running).length;
    const done = members.filter((c) => c.known && !c.running).length;
    const needsInput = members.some((c) => c.needsInput);
    return { members, running, done, needsInput };
  }, [crew, statuses, sessionIdByTmux]);
}
