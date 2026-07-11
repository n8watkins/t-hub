// Auto-continue on usage limit — the WATCHER LOGIC (armed once by
// autoContinueMount at startup, like voiceAnnounce is armed by voiceAnnounceMount).
// This module has NO import side effect, so its gate + recovery logic unit-tests
// in isolation (see autoContinueWatch.test.ts); installAutoContinue() is the arm.
//
// DEFAULT ON fleet-wide (opt-out per tile via store/autoContinue). Every watched
// terminal is recovered automatically when its agent runs out of usage, via TWO
// independent triggers into the SAME recovery action:
//
//   1. PANE-TEXT trigger — poll each watched Claude tile's visible pane
//      (readTerminalTailText, read-only) and match the on-screen usage-limit
//      MODAL (matchesUsageLimitDialog). Edge-triggered: fires ONCE when the
//      dialog appears, not every poll, and re-arms after it clears. This is the
//      trigger the old feature lacked — it dismisses the actual dialog.
//   2. STATUSLINE/CODEX TIMER trigger — the pre-existing path: when a rate-limit
//      window is EXHAUSTED (used % at its cap) and carries a reset time, wait
//      until that reset (+ a short grace) and recover. Retained for when
//      `rate_limits` data is present (Claude) / Codex account usage says so.
//
// RECOVERY (both triggers) = ESC then the continue text (buildRecoverySteps):
// ESC is sent as its OWN write and, after a settle delay, the continue text — so
// ESC dismisses the modal's numbered menu WITHOUT ever letting a trailing CR land
// on a paid option, then the continue text resumes the turn. See lib/usageLimit
// for the guardrail and recover() for the split write.
//
// On a recovery of a BLOCKED session we emit an ATTRIBUTION notification naming
// the captain/ship the tile belongs to (lib/captainAttribution).
//
// Imperative store subscriptions (no React). An app restart mid-wait re-arms
// naturally — a reset already in the past yields a ~0 delay and fires promptly.
import { useSupervision } from "../store/supervision";
import { useAutoContinue } from "../store/autoContinue";
import { useSettings } from "../store/settings";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { sessionNameForTerminal } from "../store/sessionContext";
import { clientForTerminal } from "../store/clientType";
import { deliverAgentInput } from "../ipc/client";
import { codexUsage, type CodexUsage } from "../ipc/codex";
import type { StatusSnapshot } from "../ipc/model";
import type { TerminalId } from "../ipc/types";
import { tlog } from "./diag";
import { isSatellite } from "./windows";
import { readTerminalTailText } from "./terminalTail";
import { matchesUsageLimitDialog, buildRecoverySteps } from "./usageLimit";
import { captainSubjectForSession } from "./captainAttribution";
import { notify } from "./notify";

// A window counts as EXHAUSTED (the session has actually run out, not merely "near
// the cap") at/above this used %. Higher than supervision's RATE_LIMIT_THRESHOLD
// (90, used for the soft "rateLimited" badge) so we only auto-continue when usage
// is genuinely spent and a reset is what unblocks it.
const EXHAUSTED_PCT = 99;
// Wait this long PAST the reset before injecting, so the window is definitively
// open again when the agent reads the continue.
const GRACE_MS = 5000;
// How often to scan watched Claude tiles' pane text for the usage-limit modal.
// Read-only viewport reads — cheap enough to poll frequently, frequent enough to
// notice a modal promptly.
const PANE_SCAN_MS = 4000;
// After the standalone ESC dismiss, wait this long before typing the continue
// text. Comfortably exceeds a TUI's ESC-timeout / a render settle, so the ESC is
// consumed as a lone Escape (dismiss) and never folded into a Meta/CSI sequence
// with the bytes that follow. See recover() for why this is split from the text.
export const RECOVERY_ESC_SETTLE_MS = 120;

/** A cancellable-free sleep for the split-write recovery. */
const delay = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

interface Pending {
  resetsAt: number; // unix seconds we're waiting on
  timer: ReturnType<typeof setTimeout>;
}
/** terminalId -> the armed TIMER wait (one at a time). */
const pending = new Map<TerminalId, Pending>();
/** terminalId -> the last resetsAt we already recovered for. SHARED by both
 *  triggers, keyed by the reset window, so a window is recovered exactly ONCE no
 *  matter which trigger (pane text or timer) sees it first — no double-inject, and
 *  a flapping modal on the same window never re-fires. Cleared when the window
 *  clears (evaluate), so the NEXT exhaustion re-arms. */
const handled = new Map<TerminalId, number>();

/** Every terminal currently WATCHED (exists in the workspace AND not opted out).
 *  Default ON: a tile is watched unless the user opted it out. */
function watchedTerminals(): TerminalId[] {
  const ac = useAutoContinue.getState();
  return Object.keys(useWorkspace.getState().terminals).filter((id) =>
    ac.isWatched(id),
  );
}

/** A human label for a tile (for notifications): the same derivation the tile
 *  header uses (rename → command·dir → short id). */
function tileLabel(id: TerminalId): string {
  const ws = useWorkspace.getState();
  const info = ws.terminals[id];
  return deriveLabel({
    id,
    label: ws.labels[id],
    title: info?.title,
    cwd: info?.cwd,
  });
}

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
 *  its statusline snapshot. */
function resetForTerminal(id: TerminalId): number | null {
  if (clientForTerminal(id) === "codex") return codexExhaustedReset();
  const sup = useSupervision.getState();
  const sessionId = sup.sessionIdByTmux[sessionNameForTerminal(id)];
  return exhaustedReset(sessionId ? sup.snapshots[sessionId] : undefined);
}

/** The reset window (unix s) this tile is INDEPENDENTLY confirmed blocked-past, or
 *  null. Confirmation comes from the supervision statusline snapshot (NOT the pane
 *  text): the session must be exhausted AND its reset already elapsed. This is the
 *  gate the pane trigger must pass, and its return doubles as the per-window dedup
 *  key.
 *
 *  It is the cross-check the on-screen text alone cannot provide. A HEALTHY tile
 *  that merely DISPLAYS the modal's wording (a test fixture, a `gh pr diff`, docs,
 *  a pasted dialog, a review report quoting it) is not exhausted per supervision,
 *  so this returns null and text-as-DATA can NEVER trigger a recovery. Requiring
 *  the reset to have passed also closes the dismiss/retry LOOP: a still-blocked,
 *  pre-reset window is not recovered early (which would just re-hit the limit and
 *  re-show the modal every scan). It reuses the exact exhaustion + reset data the
 *  TIMER trigger already waits on, so both triggers agree on when a session is
 *  genuinely blocked. */
function recoverableWindow(id: TerminalId): number | null {
  const resetsAt = resetForTerminal(id);
  if (resetsAt === null) return null; // not exhausted per supervision
  return Date.now() >= resetsAt * 1000 + GRACE_MS ? resetsAt : null; // reopened?
}

function cancel(id: TerminalId): void {
  const p = pending.get(id);
  if (p) {
    clearTimeout(p.timer);
    pending.delete(id);
  }
}

/** The single recovery action for BOTH triggers: dismiss the modal (ESC) and
 *  inject the continue text, then emit the captain-attributed resume
 *  notification. Best-effort: a closed tile just rejects the write.
 *
 *  SPLIT WRITE (the billing guardrail's end-to-end half): the ESC is sent as its
 *  OWN PTY write, THEN — after RECOVERY_ESC_SETTLE_MS — the continue text + CR is
 *  sent as a SECOND write. This removes the ESC-then-CR atomicity assumption
 *  entirely: with a settle gap the TUI parses the ESC as a standalone Escape (a
 *  dismiss), so it can never fold ESC into a Meta/CSI sequence and let the
 *  trailing CR land on the default-highlighted PAID option. The two writes are
 *  byte-identical to buildRecoveryInput concatenated; only the timing differs.
 *  buildRecoverySteps also STRIPS interior control chars, so the only ESC is this
 *  dismiss and the only CR is the final submit. #46 injected the equivalent
 *  `"\x1b" + text + "\r"` as one atomic write — the exact case this splits. */
async function recover(id: TerminalId, reason: string): Promise<void> {
  if (!useAutoContinue.getState().isWatched(id)) return; // opted out meanwhile
  const text = (useSettings.getState().autoContinueText || "continue").trim();
  const { dismiss, submit } = buildRecoverySteps(text);
  tlog(
    "autocontinue",
    `recovering ${id} (${reason}): ESC (settle ${RECOVERY_ESC_SETTLE_MS}ms) + "${text}"`,
  );
  try {
    // comms-plane Phase 1: auto-continue is AUTOMATION input, so it funnels through
    // the plane (`deliverAgentInput`), not the human `writeTerminal` path. The
    // billing guardrail is UNCHANGED: the ESC dismiss and the continue-text submit
    // stay two separate writes with the settle gap between them (buildRecoverySteps
    // still strips interior control chars), so we Esc+continue and NEVER select a
    // paid option (design M10: the auto-continue migration may Esc+continue only).
    // 1) ESC ALONE — an unambiguous standalone dismiss.
    await deliverAgentInput(id, dismiss, "auto-continue");
    // 2) Let the terminal settle so ESC is not folded into what follows.
    await delay(RECOVERY_ESC_SETTLE_MS);
    // 3) The continue text + Enter at the now-freed prompt (empty -> dismiss-only,
    //    so we never send a stray Enter that could re-select a menu option).
    if (submit) {
      if (!useAutoContinue.getState().isWatched(id)) return; // opted out mid-delay
      await deliverAgentInput(id, submit, "auto-continue");
    }
  } catch {
    /* terminal gone — ignore, and skip the resume notification below */
    return;
  }
  notifyResumed(id, text);
}

/** Notify the general that a BLOCKED session was auto-resumed, naming the
 *  captain/ship it belongs to. Attribution-first title (falls back to the tile
 *  label when the tile has no captain).
 *
 *  CLASS = "attention" — #44's strict chime policy reserves the `error` kind for a
 *  BLOCKER (a hard stop that ends the run). A session T-Hub already RECOVERED is
 *  not that: it's an informational "this captain was blocked and is now moving
 *  again" cue that wants the general's eyes (WHICH captain was stuck) WITHOUT
 *  sounding a blocker alarm. So it rides the softer `attention` chime, not the
 *  loud `error` one — which on any edge would be an actively misleading alarm for
 *  a self-healed session. We only WIRE this cue into #44's existing classes; we do
 *  not add or alter a class.
 *
 *  Attribution comes from #44's canonical captainAttribution
 *  (captainSubjectForSession), which keys off the CLAUDE SESSION id, so we resolve
 *  this tile's session id first; a non-captain tile resolves to null and the tile
 *  label stands. */
function notifyResumed(id: TerminalId, text: string): void {
  const label = tileLabel(id);
  const sessionId =
    useSupervision.getState().sessionIdByTmux[sessionNameForTerminal(id)];
  const attribution = sessionId ? captainSubjectForSession(sessionId) : null;
  notify(
    "attention",
    `${attribution ?? label} auto-resumed`,
    `${label} hit its usage limit; T-Hub dismissed the dialog and sent "${text}".`,
  );
}

// ---------------------------------------------------------------------------
// Trigger 1 — pane-text (the on-screen usage-limit modal)
// ---------------------------------------------------------------------------

/** Scan every watched CLAUDE tile's visible pane for the usage-limit modal and
 *  recover — but ONLY when supervision independently confirms the session is
 *  genuinely blocked past its reset (recoverableWindow). The pane text is a
 *  NECESSARY signal, never a SUFFICIENT one: without the gate, a healthy tile that
 *  merely displays the modal's wording would be injected into. Codex tiles are
 *  excluded — the modal wording is Claude's; Codex stays on the timer trigger.
 *
 *  Dedup is via the shared `handled` map keyed by the reset window, so a modal
 *  that stays up (or flaps) across scans recovers exactly once, and the timer
 *  trigger won't also fire the same window. */
export function scanPanes(): void {
  const watched = new Set(watchedTerminals());
  for (const id of watched) {
    if (clientForTerminal(id) === "codex") continue;
    if (!matchesUsageLimitDialog(readTerminalTailText(id))) continue;
    // GATE: pane text present, but is the session ACTUALLY out of usage per
    // supervision, and past its reset? If not, this is text-as-DATA — never inject.
    const window = recoverableWindow(id);
    if (window === null) continue;
    if (handled.get(id) === window) continue; // already recovered this window
    handled.set(id, window);
    void recover(id, "pane-dialog");
  }
}

// ---------------------------------------------------------------------------
// Trigger 2 — statusline / Codex reset timer
// ---------------------------------------------------------------------------

function fireTimer(id: TerminalId, resetsAt: number): void {
  pending.delete(id);
  handled.set(id, resetsAt); // never re-inject for this same window (either trigger)
  void recover(id, `timer reset ${resetsAt}`);
}

function evaluate(): void {
  const watched = new Set(watchedTerminals());

  // Drop any armed wait / handled marker for a terminal no longer watched or
  // gone (isWatched is true for any non-opted-out id, so gate on the live set).
  for (const id of [...pending.keys()]) if (!watched.has(id)) cancel(id);
  for (const id of [...handled.keys()]) if (!watched.has(id)) handled.delete(id);

  for (const id of watched) {
    const resetsAt = resetForTerminal(id);

    if (resetsAt === null) {
      // Not exhausted (cleared, resumed, or never hit) — drop any pending wait
      // and forget the handled marker so a FUTURE exhaustion re-arms cleanly.
      cancel(id);
      handled.delete(id);
      continue;
    }
    if (handled.get(id) === resetsAt) continue; // already fired for this window
    const existing = pending.get(id);
    if (existing && existing.resetsAt === resetsAt) continue; // already armed
    if (existing) clearTimeout(existing.timer);

    const delay = Math.max(0, resetsAt * 1000 + GRACE_MS - Date.now());
    const timer = setTimeout(() => fireTimer(id, resetsAt), delay);
    pending.set(id, { resetsAt, timer });
    tlog(
      "autocontinue",
      `armed ${id}: reset ${resetsAt}, firing in ${Math.round(delay / 1000)}s`,
    );
  }
}

let installed = false;
export function installAutoContinue(): void {
  // Satellites (popped-out tab windows) load the same bundle; the auto-continue
  // watcher must run in exactly ONE window or every open window recovers the
  // same tile and double-injects. The main window owns it.
  if (isSatellite()) return;
  if (installed) return;
  installed = true;

  // Timer trigger: re-evaluate whenever a snapshot/status lands, the opt-out set
  // changes, or the workspace terminal set changes (a new default-ON tile).
  useSupervision.subscribe(evaluate);
  useAutoContinue.subscribe(evaluate);
  useWorkspace.subscribe(evaluate);

  // Codex has NO event stream (its usage lives in session files), so poll it and
  // re-evaluate. Adopt only good readings (keep last-known on a failed poll).
  const pollCodex = (): void => {
    // Only hit the (file-reading) codex_usage command when at least one WATCHED
    // terminal is actually a Codex tile.
    const anyCodex = watchedTerminals().some(
      (id) => clientForTerminal(id) === "codex",
    );
    if (!anyCodex) return;
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

  // Pane trigger: poll the visible modal. Independent of the event-driven timer
  // path, since terminal output is not delivered through a store subscription.
  scanPanes();
  setInterval(scanPanes, PANE_SCAN_MS);

  evaluate(); // initial pass (covers app-restart-mid-wait)
}

// ---------------------------------------------------------------------------
// Test seams (never used in production; the Mount + intervals are not exercised
// in unit tests — scanPanes() is driven directly against seeded store state).
// ---------------------------------------------------------------------------

/** Clear all armed waits / handled markers / cached Codex usage so each test
 *  starts from a clean watcher. Does NOT touch the stores (tests own those). */
export function _resetAutoContinueForTest(): void {
  for (const p of pending.values()) clearTimeout(p.timer);
  pending.clear();
  handled.clear();
  latestCodex = null;
}
