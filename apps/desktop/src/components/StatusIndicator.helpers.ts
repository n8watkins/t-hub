import type { SessionStatus } from "../ipc/model";
import type { TerminalState } from "../ipc/types";

export type StatusVariant = "working" | "idle" | "done" | "attention" | "error";

/**
 * Map a Claude/Codex session's reducer status (FR-012) to an indicator variant.
 * This is the PRECISE signal - distinct states for actively working vs. asking a
 * question / needing permission vs. idle - so it never conflates them the way a
 * raw output-activity pulse does.
 */
export function sessionStatusToVariant(status: SessionStatus): StatusVariant {
  switch (status) {
    case "working":
    case "waitingOnSubagents":
      return "working";
    case "needsQuestion":
    case "needsPermission":
    case "rateLimited":
      return "attention";
    case "failed":
      return "error";
    case "completed":
      return "done";
    // detached / restoring / expired / unknown → present but quiet.
    default:
      return "idle";
  }
}

/**
 * The indicator variant for ONE terminal row/tile, or `null` for a BLANK state
 * (an empty/quiet terminal - no agent bound and nothing running).
 *
 * Priority: a lifecycle error wins; then, if an agent session is bound (Claude
 * has supervision + statusline), trust its precise reducer status - crucially we
 * do NOT fall back to the noisy output-activity proxy here, so an idle Claude TUI
 * that keeps redrawing no longer false-pulses as "working". Only a SESSION-LESS
 * terminal (a plain shell, or Codex before/without a status snapshot) uses output
 * activity: it pulses while a command is actively producing output, and is blank
 * when quiet.
 */
export function terminalVariant(
  state: TerminalState,
  sessionStatus: SessionStatus | undefined,
  outputActive: boolean,
): StatusVariant | null {
  if (state === "error") return "error";
  if (sessionStatus !== undefined) return sessionStatusToVariant(sessionStatus);
  if (outputActive) return "working";
  return null;
}
