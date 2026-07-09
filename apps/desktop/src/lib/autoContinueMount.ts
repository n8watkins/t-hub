// Auto-continue on usage limit — installed once at startup (idempotent side-effect
// import from main.tsx, like notifyMount/statusMount).
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
// RECOVERY (both triggers) = ESC then the continue text (buildRecoveryInput):
// ESC dismisses the modal's numbered menu WITHOUT selecting a paid option, then
// the continue text resumes the turn. See lib/usageLimit for the guardrail.
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
import { writeTerminal } from "../ipc/client";
import { codexUsage, type CodexUsage } from "../ipc/codex";
import type { StatusSnapshot } from "../ipc/model";
import type { TerminalId } from "../ipc/types";
import { tlog } from "./diag";
import { isSatellite } from "./windows";
import { readTerminalTailText } from "./terminalTail";
import { matchesUsageLimitDialog, buildRecoveryInput } from "./usageLimit";
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

interface Pending {
  resetsAt: number; // unix seconds we're waiting on
  timer: ReturnType<typeof setTimeout>;
}
/** terminalId -> the armed TIMER wait (one at a time). */
const pending = new Map<TerminalId, Pending>();
/** terminalId -> the last resetsAt we already recovered for (timer path fires
 *  once per window). */
const handled = new Map<TerminalId, number>();
/** Terminals whose pane currently shows the modal — the pane trigger is EDGE
 *  triggered off this set, so one dialog fires once (and a later, distinct dialog
 *  re-fires once it re-appears). */
const paneActive = new Set<TerminalId>();

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

function cancel(id: TerminalId): void {
  const p = pending.get(id);
  if (p) {
    clearTimeout(p.timer);
    pending.delete(id);
  }
}

/** The single recovery action for BOTH triggers: dismiss the modal (ESC) and
 *  inject the continue text, then emit the captain-attributed resume
 *  notification. Best-effort: a closed tile just rejects the write. */
function recover(id: TerminalId, reason: string): void {
  if (!useAutoContinue.getState().isWatched(id)) return; // opted out meanwhile
  const text = (useSettings.getState().autoContinueText || "continue").trim();
  // ONE recovery sequence for both triggers, from the single source
  // usageLimit.ts::buildRecoveryInput: ESC FIRST (dismiss the modal's numbered
  // menu without ever selecting a paid "Add funds / Switch to Team" option), then
  // the continue text + Enter so the agent resumes once its window resets. This is
  // the fleet's binding doctrine — the automation must ONLY ever take the
  // Esc+continue path. #46 injected the equivalent `"\x1b" + text + "\r"` inline;
  // we converge on buildRecoveryInput so there is no divergent copy (a blank text
  // safely collapses to ESC-alone there, never a stray digit/Enter).
  const input = buildRecoveryInput(text);
  tlog("autocontinue", `recovering ${id} (${reason}): ESC + "${text}"`);
  void writeTerminal(id, input).catch(() => {
    /* terminal gone — ignore */
  });
  notifyResumed(id, text);
}

/** Notify the general that a BLOCKED session was auto-resumed, naming the
 *  captain/ship it belongs to. Attribution-first title (falls back to the tile
 *  label when the tile has no captain).
 *
 *  CLASS = "error" — #44's strict chime policy reserves the `error` kind for a
 *  BLOCKER (a hard stop that ends the run). A usage-limit lockout IS that blocker,
 *  so the auto-resume cue rides #44's blocker chime rather than the softer
 *  "attention" (decision-needed) tone: the general hears the fleet-blocking event
 *  distinctly, even though T-Hub already cleared it. We only WIRE the new cue into
 *  #44's existing classes; we do not add or alter a class.
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
    "error",
    `${attribution ?? label} auto-resumed`,
    `${label} hit its usage limit; T-Hub dismissed the dialog and sent "${text}".`,
  );
}

// ---------------------------------------------------------------------------
// Trigger 1 — pane-text (the on-screen usage-limit modal)
// ---------------------------------------------------------------------------

/** Scan every watched CLAUDE tile's visible pane for the usage-limit modal and
 *  recover on the RISING edge (dialog appeared). Codex tiles are excluded — the
 *  modal wording is Claude's; Codex stays on the timer trigger. */
function scanPanes(): void {
  const watched = new Set(watchedTerminals());
  // Forget stale edge state for tiles no longer watched/present, so a future
  // dialog on a re-created id fires cleanly.
  for (const id of [...paneActive]) if (!watched.has(id)) paneActive.delete(id);

  for (const id of watched) {
    if (clientForTerminal(id) === "codex") continue;
    const present = matchesUsageLimitDialog(readTerminalTailText(id));
    if (present) {
      if (!paneActive.has(id)) {
        paneActive.add(id); // rising edge — recover once
        recover(id, "pane-dialog");
      }
    } else if (paneActive.has(id)) {
      paneActive.delete(id); // dialog cleared — re-arm for the next one
    }
  }
}

// ---------------------------------------------------------------------------
// Trigger 2 — statusline / Codex reset timer
// ---------------------------------------------------------------------------

function fireTimer(id: TerminalId, resetsAt: number): void {
  pending.delete(id);
  handled.set(id, resetsAt); // never re-inject for this same window
  recover(id, `timer reset ${resetsAt}`);
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
function installAutoContinue(): void {
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

installAutoContinue();
