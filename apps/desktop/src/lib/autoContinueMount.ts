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
import { clientForTerminal } from "../store/clientType";
import { writeTerminal } from "../ipc/client";
import { codexUsage, type CodexUsage } from "../ipc/codex";
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

/** Latest account-level Codex usage, refreshed by a poll in installAutoContinue
 *  (Codex has no event stream like Claude's statusline). null until first poll. */
let latestCodex: CodexUsage | null = null;

/** Soonest reset (unix s) among EXHAUSTED Codex windows, or null. Codex usage is
 *  account-wide, so every Codex tile shares this reading. */
function codexExhaustedReset(): number | null {
  const u = latestCodex;
  if (!u || !u.ok) return null;
  const resets: number[] = [];
  for (const w of [u.primary, u.secondary]) {
    if (
      w &&
      (w.usedPercent ?? 0) >= EXHAUSTED_PCT &&
      typeof w.resetsAt === "number"
    ) {
      resets.push(w.resetsAt);
    }
  }
  return resets.length ? Math.min(...resets) : null;
}

/** The reset time to wait on for terminal `id`, resolved by WHICH agent it runs:
 *  a Codex tile uses the account-level Codex usage; everything else (Claude) uses
 *  its statusline snapshot. The continue INJECTION is agent-agnostic (just typing
 *  into the PTY) — only this "ran out + resets when" detection differs by agent. */
function resetForTerminal(id: TerminalId): number | null {
  if (clientForTerminal(id) === "codex") return codexExhaustedReset();
  const sup = useSupervision.getState();
  const sessionId = sup.sessionIdByTmux[sessionNameForTerminal(id)];
  return exhaustedReset(sessionId ? sup.snapshots[sessionId] : undefined);
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
  const enabled = useAutoContinue.getState().enabled;

  // Drop any armed wait for a terminal that's no longer opted in.
  for (const id of [...pending.keys()]) {
    if (!enabled[id]) cancel(id);
  }

  for (const id of Object.keys(enabled)) {
    const resetsAt = resetForTerminal(id);

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
  // Claude is event-driven: re-evaluate whenever a snapshot/status lands or the
  // opt-in set changes.
  useSupervision.subscribe(evaluate);
  useAutoContinue.subscribe(evaluate);
  // Codex has NO event stream (its usage lives in session files), so poll it and
  // re-evaluate. The precise wait is still a per-window timer; this poll only has
  // to be frequent enough to NOTICE a Codex session ran out. Adopt only good
  // readings (keep last-known on a failed poll), like the sidebar usage strip.
  const pollCodex = (): void => {
    void codexUsage()
      .then((u) => {
        if (u && u.ok) {
          latestCodex = u;
          evaluate();
        }
      })
      .catch(() => {
        /* transient — keep last-known */
      });
  };
  pollCodex();
  setInterval(pollCodex, 2 * 60 * 1000);
  evaluate(); // initial pass (covers app-restart-mid-wait)
}

installAutoContinue();
