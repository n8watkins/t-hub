// Auto-continue on usage reset — installed once at startup (idempotent side-effect
// import from main.tsx, like notifyMount/statusMount).
//
// For each terminal the user opted into (useAutoContinue), we watch its Claude
// session's statusline snapshot (resolved via the supervision sessionIdByTmux
// index). When a rate-limit window is EXHAUSTED (used % at its cap) and carries a
// known reset time, we wait until that reset (+ a short grace) and inject the
// continue command into the PTY, so a session that ran out of usage resumes on
// its own. Fires once per (terminal, resetsAt) window.
//
// Imperative store subscriptions (no React) with one timer per terminal; we
// re-evaluate on every supervision / opt-in change. An app restart mid-wait
// re-arms naturally — a reset already in the past yields a ~0 delay and fires
// promptly on the next snapshot.
import { useSupervision } from "../store/supervision";
import { useAutoContinue } from "../store/autoContinue";
import { useSettings } from "../store/settings";
import { sessionNameForTerminal } from "../store/sessionContext";
import { writeTerminal } from "../ipc/client";
import type { StatusSnapshot } from "../ipc/model";
import type { TerminalId } from "../ipc/types";
import { tlog } from "./diag";

// A window counts as EXHAUSTED (the session has actually run out, not merely "near
// the cap") at/above this used %. Higher than supervision's RATE_LIMIT_THRESHOLD
// (90, used for the soft "rateLimited" badge) so we only auto-continue when usage
// is genuinely spent and a reset is what unblocks it.
const EXHAUSTED_PCT = 99;
// Wait this long PAST the reset before injecting, so the window is definitively
// open again when the agent reads the continue.
const GRACE_MS = 5000;

interface Pending {
  resetsAt: number; // unix seconds we're waiting on
  timer: ReturnType<typeof setTimeout>;
}
/** terminalId -> the armed wait (one at a time). */
const pending = new Map<TerminalId, Pending>();
/** terminalId -> the last resetsAt we already injected for (fire once per window). */
const handled = new Map<TerminalId, number>();

/** The soonest reset time (unix s) among EXHAUSTED windows in this snapshot, or
 *  null when nothing is exhausted / no reset time is known. */
function exhaustedReset(snap: StatusSnapshot | undefined): number | null {
  if (!snap || !snap.rateLimitsPresent) return null;
  const resets: number[] = [];
  for (const w of [snap.fiveHour, snap.sevenDay]) {
    if (
      w &&
      (w.usedPercentage ?? 0) >= EXHAUSTED_PCT &&
      typeof w.resetsAt === "number"
    ) {
      resets.push(w.resetsAt);
    }
  }
  return resets.length ? Math.min(...resets) : null;
}

function cancel(id: TerminalId): void {
  const p = pending.get(id);
  if (p) {
    clearTimeout(p.timer);
    pending.delete(id);
  }
}

function fire(id: TerminalId, resetsAt: number): void {
  pending.delete(id);
  handled.set(id, resetsAt); // never re-inject for this same window
  if (!useAutoContinue.getState().enabled[id]) return; // toggled off while waiting
  const text = (useSettings.getState().autoContinueText || "continue").trim();
  if (!text) return;
  tlog("autocontinue", `injecting "${text}" into ${id} (reset ${resetsAt})`);
  // Type the command + Enter into the PTY. The session was blocked on the limit,
  // so this lands at its prompt and resumes the turn. Best-effort: a closed tile
  // just rejects the write.
  void writeTerminal(id, text + "\r").catch(() => {
    /* terminal gone — ignore */
  });
}

function evaluate(): void {
  const sup = useSupervision.getState();
  const enabled = useAutoContinue.getState().enabled;

  // Drop any armed wait for a terminal that's no longer opted in.
  for (const id of [...pending.keys()]) {
    if (!enabled[id]) cancel(id);
  }

  for (const id of Object.keys(enabled)) {
    const sessionId = sup.sessionIdByTmux[sessionNameForTerminal(id)];
    const snap = sessionId ? sup.snapshots[sessionId] : undefined;
    const resetsAt = exhaustedReset(snap);

    if (resetsAt === null) {
      // Not exhausted (cleared, resumed, or never hit) — drop any pending wait and
      // forget the handled marker so a FUTURE exhaustion re-arms cleanly.
      cancel(id);
      handled.delete(id);
      continue;
    }
    if (handled.get(id) === resetsAt) continue; // already fired for this window
    const existing = pending.get(id);
    if (existing && existing.resetsAt === resetsAt) continue; // already armed
    if (existing) clearTimeout(existing.timer);

    const delay = Math.max(0, resetsAt * 1000 + GRACE_MS - Date.now());
    const timer = setTimeout(() => fire(id, resetsAt), delay);
    pending.set(id, { resetsAt, timer });
    tlog(
      "autocontinue",
      `armed ${id}: reset ${resetsAt}, firing in ${Math.round(delay / 1000)}s`,
    );
  }
}

let installed = false;
function installAutoContinue(): void {
  if (installed) return;
  installed = true;
  // Re-evaluate whenever a snapshot/status lands or the opt-in set changes.
  useSupervision.subscribe(evaluate);
  useAutoContinue.subscribe(evaluate);
  evaluate(); // initial pass (covers app-restart-mid-wait)
}

installAutoContinue();
