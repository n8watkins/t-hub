// Terminal TUIs cannot acknowledge a safe continuation. Quota recovery is an
// alert until a structured runtime can bind the operation to an exact turn.
// Keep the persisted per-tile preference so existing opt-outs remain respected.
import { useSupervision } from "../store/supervision";
import { useAutoContinue } from "../store/autoContinue";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { sessionNameForTerminal } from "../store/sessionContext";
import { clientForTerminal } from "../store/clientType";
import type { StatusSnapshot } from "../ipc/model";
import { isSatellite } from "./windows";
import { readTerminalTailText } from "./terminalTail";
import { matchesUsageLimitDialog } from "./usageLimit";
import { captainSubjectForSession } from "./captainAttribution";
import { notify } from "./notify";

const MAX_EVIDENCE_AGE_MS = 3 * 60 * 1000;
const handled = new Map<string, string>();

/** All exhausted windows must have known, finite reset times. */
export function exhaustedReset(snap: StatusSnapshot | undefined): number | null {
  if (!snap?.rateLimitsPresent) return null;
  const resets: number[] = [];
  for (const window of [snap.fiveHour, snap.sevenDay]) {
    if ((window?.usedPercentage ?? 0) < 99) continue;
    const reset = window?.resetsAt;
    if (typeof reset !== "number" || !Number.isFinite(reset) || reset <= 0) return null;
    resets.push(reset);
  }
  return resets.length ? Math.max(...resets) : null;
}

/** Never infer a session-level action from account-wide Codex usage. */
export function scanPanes(): void {
  const ws = useWorkspace.getState();
  const sup = useSupervision.getState();
  for (const id of handled.keys()) if (!ws.terminals[id]) handled.delete(id);
  for (const [id, info] of Object.entries(ws.terminals)) {
    if (!useAutoContinue.getState().isWatched(id)) continue;
    if (clientForTerminal(id) !== "claude") continue;
    const sessionId = sup.sessionIdByTmux[sessionNameForTerminal(id)];
    const snapshot = sup.snapshots[sessionId];
    const age = Date.now() - (snapshot?.ingestedAtMs ?? 0);
    if (age < 0 || age > MAX_EVIDENCE_AGE_MS) continue;
    const reset = exhaustedReset(snapshot);
    if (reset === null || Date.now() < reset * 1000 + 5000) continue;
    if (!matchesUsageLimitDialog(readTerminalTailText(id))) continue;
    const key = `${sessionId}:${reset}`;
    if (handled.get(id) === key) continue;
    const label = deriveLabel({ id, label: ws.labels[id], title: info.title, cwd: info.cwd });
    notify("attention", `${captainSubjectForSession(sessionId) ?? label}: usage reset`,
      "The reported usage window has reset. Review the terminal and resume manually; terminal recovery does not send keystrokes.");
    handled.set(id, key);
  }
}

let installed = false;
export function installAutoContinue(): void {
  if (isSatellite() || installed) return;
  installed = true;
  scanPanes();
  setInterval(scanPanes, 4000);
}

export function _resetAutoContinueForTest(): void {
  handled.clear();
}
